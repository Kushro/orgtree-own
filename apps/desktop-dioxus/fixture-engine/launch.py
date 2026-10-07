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

Desk completo (#27), solo en este fixture:

- Al final del historial hay un mensaje con mail (con respuesta citada y un
  adjunto), avisos y texto en segmentos (la proyección de ``read_chat``), y una
  respuesta con pensamiento y enlaces a archivos locales: uno que existe (junto
  a la raíz descartable) y otro que no.
- El envío pasa por la puerta real (``send_message`` y la admisión de halt),
  pero sin proveedor: el mail queda guardado y el turno no arranca. Un agente
  detenido lo retiene hasta reanudarlo, como el motor real.
- ``POST /api/fixture/turn-state`` ``{node, state}`` simula el estado del turno
  (``idle``, ``queued`` detrás del límite de turnos, ``working`` con una
  respuesta en curso) y avisa por el WebSocket, para probar los estados del desk.

Ciclo de vida del motor (#25), como el fixture del spike de Tauri (#19):

- ``ORGTREE_FIXTURE_CONVERT=N`` simula la conversión de la primera ejecución:
  N checkpoints ``database-convert: …`` separados por
  ``ORGTREE_FIXTURE_CONVERT_GAP`` segundos (2,5 por defecto), por el mismo
  reportero de progreso que usa ``engine/pg_process.py``. Con ``fail``, el
  último paso levanta ``ConversionFailed`` y el motor imprime el rechazo
  ``conversion-failed``. Solo en Windows (``launch.main``).
- ``POST /api/fixture/maintenance`` (``{"action": "restart" | "update"}``)
  crea un pedido de mantenimiento del motor, como lo haría un agente con
  ``orgtree_self_relaunch``.
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
# Archivos para los enlaces del desk (#27): junto a la raíz descartable, nunca adentro.
FILES = _DATA.parent / f"{_DATA.name}-fixture-files"
REPORT = FILES / "informe.txt"
MISSING = (Path("C:/orgtree-fixture-no-existe/falta.log") if os.name == "nt"
           else Path("/orgtree-fixture-no-existe/falta.log"))
RICH_AT = "2026-10-07T12:00:00Z"
RICH_RAW = "FROM @user (the user) · message · 2026-10-07T12:00:00Z\nRevisá el informe adjunto."


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
                     {"type": "text", "text": "Orgtree organiza agentes de código en un **organigrama**.\n\n"
                      "- desk en vivo\n- `README.md` leído\n\n<img src=x onerror=\"document.title='inyectado'\">"}]})
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
    # Desk completo (#27): segmentos con mail y avisos, pensamiento y archivos.
    mail = rec("user", parent, RICH_AT, {"role": "user", "content": RICH_RAW})
    reply = rec("assistant", mail["uuid"], "2026-10-07T12:00:05Z",
                {"id": "msg_fixture_rich", "role": "assistant", "content": [
                    {"type": "thinking", "thinking": "Pienso: el informe está adjunto; lo leo y respondo."},
                    {"type": "text", "text": "Listo. El informe está en "
                     f"[informe.txt](<{REPORT}>) y el log en [falta.log](<{MISSING}>), que no existe. "
                     "Más en [el sitio](https://example.com/orgtree) y en [un relativo](uploads/informe.txt). "
                     # #30: enlaces que la vía controlada no abre (otro esquema, con usuario)
                     "Ni [un raro](ssh://example.com/x) ni [con usuario](https://usuario:clave@example.com/)."}]})
    return records + [mail, reply]


def _rich_segments() -> list[dict]:
    """La proyección de la conversación del mensaje con mail (#27)."""
    quote = "Respuesta 2: la tarea 0 avanza."
    return [
        {"kind": "text", "text": "Revisá el informe adjunto."},
        {"kind": "mail", "rows": [
            {"id": "m-fixture-1", "from": "@user", "kind": "message", "at": RICH_AT,
             "body": "Te paso el **informe** de la semana.",
             "reply_to": {"source_event_ref": {"org": ORG, "agent": AGENT, "generation": 0,
                                               "eventId": "fixture-reply-target"},
                          "quoted_context": quote},
             "attachments": [{"name": "informe.txt", "path": "uploads/informe.txt", "bytes": 42}]},
            {"id": "m-fixture-2", "from": "jefe", "kind": "status", "at": RICH_AT,
             "body": "Avance: la revisión va por la mitad."},
        ]},
        {"kind": "notices", "rows": [
            {"at": RICH_AT, "text": "Aviso: el límite de turnos subió a 16."},
        ]},
    ]


def _stub_providers() -> None:
    from orgtree import accounts, providers, supervisor, warmpool
    # #30: con la forma entera de un CLI ausente, porque `/api/providers` la lee
    absent = {"available": False, "installed": False, "path": None, "source": "", "version": None,
              "connected": False, "email": None, "kind": None}
    providers.antigravity_status = lambda **kw: {**absent, "models": []}
    providers.codex_status = lambda **kw: {**absent, "codex_home": ""}
    # Proveedor stub para contratar (#26): la puerta de contratación
    # (`provider_hire_gate`) pide el CLI de Claude instalado y una cuenta
    # iniciada. En el CI no hay ninguno de los dos, así que el fixture los
    # declara presentes. Ningún turno corre: `send_message` y el pool de
    # procesos calientes están apagados abajo.
    supervisor.claude_install_state = lambda force=False: {"installed": True, "path": "fixture-claude", "source": "path"}
    supervisor.cli_version = lambda: "fixture"
    accounts.live_identity = lambda: {"uuid": "fixture-account", "email": ""}
    supervisor.start_usage_warm_loop = lambda: None
    supervisor.start_cred_watcher = lambda: None
    warmpool.start_warm_pool = lambda: None
    # Desk completo (#27): el envío pasa por la puerta real (`send_message`, con
    # la admisión de halt, que retiene el mail de un agente detenido), pero el
    # cuerpo no arranca ningún turno: el mail queda guardado en el buzón.
    from orgtree import halt

    def admit_without_provider(slug, nid, text, *a, **k):
        busy = supervisor.state(slug, nid).get("busy")
        return {"accepted": True, "queued": 1 if busy else 0}

    supervisor._admit_message = halt.admission(
        admit_without_provider, rows=lambda slug, nid, *_a, **_k: supervisor._envelope_rows(nid))


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
    FILES.mkdir(parents=True, exist_ok=True)
    REPORT.write_text("Informe semanal del fixture.\n", encoding="utf-8")
    path.write_text("".join(json.dumps(r) + "\n" for r in _records(sid)), encoding="utf-8")
    store.save_org(org)
    # La proyección del mensaje con mail (segmentos), como la escribe el motor al admitir el turno.
    from orgtree import supervisor
    supervisor._record_prompt_view(ORG, sid, RICH_RAW, "Revisá el informe adjunto.", at=RICH_AT,
                                   segments=_rich_segments())
    # El adjunto existe en la carpeta del agente, con su ruta relativa.
    uploads = Path(supervisor.scratch_dir(ORG, AGENT)) / "uploads"
    uploads.mkdir(parents=True, exist_ok=True)
    (uploads / "informe.txt").write_text("Informe semanal del fixture.\n", encoding="utf-8")


def _install_turn_state() -> None:
    """`POST /api/fixture/turn-state` (solo en este fixture): simula el estado
    del turno de un agente y avisa por el WebSocket de la org (#27)."""
    from fastapi import HTTPException
    from orgtree import api, supervisor

    async def set_turn_state(body: dict) -> dict:
        node = str(body.get("node") or AGENT)
        state = body.get("state")
        st = supervisor.state(ORG, node)
        with supervisor._state_lock:
            for key in ("busy", "waiting", "responding"):
                st[key] = False
            st.pop("queued_for_slot", None)
            st.pop("inflight_at", None)
            if state == "queued":
                st["waiting"] = True
                st["queued_for_slot"] = {"since": time.time(), "limit": 16, "waiting": 3}
            elif state == "working":
                st["busy"] = st["responding"] = True
            elif state != "idle":
                raise HTTPException(422, "state must be idle, queued or working")
        await api.hub.node_event(ORG, node, "fixture_turn_state", {"state": state})
        return {"node": node, "state": state}

    api.app.add_api_route("/api/fixture/turn-state", set_turn_state, methods=["POST"])
    # Antes que cualquier ruta comodín de la app.
    api.app.router.routes.insert(0, api.app.router.routes.pop())


#: Lo que siembra ``POST /api/fixture/attention`` (#28), con datos reales del ledger.
ATTENTION = {
    "urgent": {"body": "El build de Windows falló dos veces seguidas.",
               "reason": "El CI está rojo: ¿reintento o lo dejo para mañana?"},
    "urgent2": {"body": "El instalador quedó en 51 MB.",
                "reason": "Pasó el límite que pusimos: ¿lo publico igual?"},
    "routine": {"body": "Terminé de leer el README; sigo con el instalador."},
    "question": {"question": "¿Publico la pre-release de Dioxus?", "header": "Release",
                 "options": [{"label": "Sí", "description": "publicarla ahora"},
                             {"label": "Todavía no", "description": "esperar al CI"}]},
    "question2": {"question": "¿Renombro la rama del spike?", "header": "Rama",
                  "options": [{"label": "Sí"}, {"label": "No"}]},
    "tickets": [("Firmar el instalador", "¿Uso el certificado de prueba o espero el real?"),
                ("Medir el arranque en frío", "¿Mido con el antivirus prendido o apagado?")],
}


def _install_attention() -> None:
    """`POST /api/fixture/attention` ``{kind}`` (solo en este fixture, #28): el
    agente ``worker`` le escribe al usuario (mail urgente o de rutina), le
    pregunta algo, o levanta la bandera de atención en dos tickets. Todo pasa
    por el ledger real (``post_mail``, ``ask_user``, ``work_create`` y
    ``work_update``), así que la bandeja, la proyección de notificaciones y el
    WebSocket ven lo mismo que con un agente de verdad."""
    from fastapi import HTTPException
    from orgtree import api, store
    from orgtree.ledger import USER

    def create(org, kind: str) -> dict:
        if kind in ("urgent", "urgent2"):
            mail = org.post_mail(AGENT, USER, ATTENTION[kind]["body"], urgent=True,
                                 urgent_reason=ATTENTION[kind]["reason"])
            return {"mail": mail.get("id")}
        if kind == "routine":
            return {"mail": org.post_mail(AGENT, USER, ATTENTION["routine"]["body"]).get("id")}
        if kind in ("question", "question2"):
            q = ATTENTION[kind]
            org.ask_user(AGENT, questions=[{"question": q["question"], "header": q["header"],
                                            "options": q["options"]}])
            return {"ask": True}
        if kind == "tickets":
            slugs = []
            for title, reason in ATTENTION["tickets"]:
                item = org.work_create(AGENT, title, objective=(
                    f"Problema: {title.lower()} bloquea la pre-release. "
                    "Solución: decidirlo con el usuario y seguir."), owner=AGENT,
                    status="in_progress")
                slug = item.get("slug") or item.get("item", {}).get("slug")
                org.work_update(AGENT, slug, done_so_far=["preparé el entorno"],
                                working_on_next=["esperar la decisión"],
                                attention=True, attention_reason=reason)
                slugs.append(slug)
            return {"tickets": slugs}
        raise HTTPException(422, "kind must be urgent, urgent2, routine, question, question2 or tickets")

    def seed(body: dict) -> dict:
        with store.write_org(ORG) as org:
            result = create(org, str(body.get("kind") or ""))
            store.save_org(org)
        return result

    api.app.add_api_route("/api/fixture/attention", seed, methods=["POST"])
    api.app.router.routes.insert(0, api.app.router.routes.pop())


#: Los tickets que siembra ``POST /api/fixture/docket`` ``{kind: "seed"}`` (#29).
DOCKET = {
    "runtime": ("Empaquetar el runtime", "in_progress"),
    "lzma": ("Comprimir con LZMA", "open"),
    "certificado": ("Esperar el certificado", "blocked"),
    "docs": ("Documentar el spike", "open"),
    "memoria": ("Medir la memoria", "backlogged"),
    "win10": ("Probar en Windows 10", "in_progress"),
}
DOCKET_BLOCKED = "Falta el certificado de firma del proveedor."
DOCKET_DROPPED = "Windows 10 quedó fuera del soporte del spike."


def _install_docket() -> None:
    """`POST /api/fixture/docket` ``{kind, ...}`` (solo en este fixture, #29).

    Todo pasa por el ledger real, como un agente con la herramienta del docket
    (``work_create``, ``work_update``, ``work_assign``, ``work_decision``,
    ``work_evidence``, ``work_artifact_record`` y ``ask_user``), así que la
    lista, el detalle y el WebSocket ven lo mismo que con agentes de verdad:

    - ``seed``: contrata a ``jefe`` y siembra seis tickets con estados, dueños,
      un sub-ítem, decisiones, evidencias, un artefacto, historial y un holder
      anterior (``runtime`` pasa de ``jefe`` a ``worker``). Uno sin dueño queda
      en el backlog (para "Staff…") y otro termina ``dropped`` (se archiva en
      el acto). Devuelve los slugs.
    - ``status`` ``{slug, status, reason?}``: el dueño cambia el estado;
    - ``flag`` ``{slug, reason}``: el dueño levanta la bandera de atención;
    - ``question`` ``{slug}``: ``worker`` adjunta una pregunta al ticket.
    """
    from fastapi import HTTPException
    from orgtree import api, store, workevidence
    from orgtree.ledger import USER

    def slug_of(result: dict) -> str:
        return str(result.get("slug") or (result.get("item") or {}).get("slug"))

    def owner_of(org, slug: str) -> str:
        item, _ = org._work_find(slug)
        return str((item.get("owner") or {}).get("node") or AGENT)

    def seed(org) -> dict:
        if "jefe" not in org.nodes:
            org.hire(USER, None, "haiku", 0, "jefe")
        slugs: dict[str, str] = {}

        def create(key: str, actor: str, owner, **extra) -> str:
            title, status = DOCKET[key]
            start = "open" if status == "dropped" else status
            result = org.work_create(actor, title, objective=(
                f"**Problema:** {title.lower()} frena la pre-release.\n\n"
                f"**Solución:** hacerlo en `{title.lower().replace(' ', '-')}` y "
                "registrar lo hecho en el docket."), owner=owner, status=start,
                done_so_far=["revisé el alcance"], working_on_next=["empezar"], **extra)
            slugs[key] = slug_of(result)
            return slugs[key]

        runtime = create("runtime", "jefe", "jefe")
        # un holder anterior: el ticket pasa de jefe a worker
        org.work_assign(USER, runtime, AGENT, notify=False)
        org.work_update(AGENT, runtime, done_so_far=["armé el runtime embebido", "medí 62,8 MB"],
                        working_on_next=["comprimir con LZMA"], status="in_progress")
        org.work_decision(AGENT, runtime, "Se usa el runtime embebido de Python 3.13, no el del sistema.")
        org.work_decision(AGENT, runtime, "LZMA sólido para el instalador: tarda 90 s pero ahorra 40 MB.")
        org.work_evidence(AGENT, runtime, "commit", "6e53697", note="runtime armado")
        org.work_evidence(AGENT, runtime, "log", "ci/runtime.log", note="digest verificado")
        # un artefacto: bytes guardados como lo hace la API, y su registro
        adir = Path(api._work_artifact_dir(ORG, runtime))
        adir.mkdir(parents=True, exist_ok=True)
        src = adir / "fuente-medicion.txt"
        src.write_text("runtime 62.8 MB\npostgresql 141.3 MB\n", encoding="utf-8")
        sha, size = workevidence.file_digest(str(src))
        stored = f"{sha.split(':', 1)[1][:12]}-medicion.txt"
        src.rename(adir / stored)
        org.work_artifact_record(AGENT, runtime, "medicion.txt", size, stored, sha)
        create("lzma", AGENT, AGENT, parent=runtime)
        create("certificado", AGENT, AGENT, blocked_reason=DOCKET_BLOCKED)
        create("docs", "jefe", "jefe")
        create("memoria", USER, None)
        win10 = create("win10", AGENT, AGENT)
        org.work_update(AGENT, win10, done_so_far=["probé el instalador"], working_on_next=["nada más"],
                        status="dropped", dropped_reason=DOCKET_DROPPED)
        return {"tickets": slugs}

    def run(body: dict) -> dict:
        kind = str(body.get("kind") or "")
        with store.write_org(ORG) as org:
            if kind == "seed":
                result = seed(org)
            elif kind == "status":
                slug, status = str(body["slug"]), str(body["status"])
                reason = body.get("reason")
                org.work_update(owner_of(org, slug), slug, done_so_far=["avancé"], working_on_next=["seguir"],
                                status=status,
                                blocked_reason=reason if status == "blocked" else None,
                                dropped_reason=reason if status == "dropped" else None)
                result = {"slug": slug, "status": status}
            elif kind == "flag":
                slug = str(body["slug"])
                org.work_update(owner_of(org, slug), slug, done_so_far=["avancé"], working_on_next=["esperar"],
                                attention=True, attention_reason=str(body.get("reason") or "¿Seguimos?"))
                result = {"slug": slug, "flagged": True}
            elif kind == "question":
                slug = str(body["slug"])
                org.ask_user(AGENT, questions=[{"question": "¿Cierro este ticket?", "header": "Cierre",
                                                "options": [{"label": "Sí"}, {"label": "No"}]}],
                             work_item=slug)
                result = {"slug": slug, "question": True}
            else:
                raise HTTPException(422, "kind must be seed, status, flag or question")
            store.save_org(org)
        return result

    api.app.add_api_route("/api/fixture/docket", run, methods=["POST"])
    api.app.router.routes.insert(0, api.app.router.routes.pop())


def _install_maintenance() -> None:
    """``POST /api/fixture/maintenance`` (#25): un pedido de mantenimiento del
    motor, por el módulo real (``desktop_maintenance.request``)."""
    from fastapi import HTTPException
    from orgtree import api

    def post_maintenance(body: dict) -> dict:
        from orgtree import desktop_maintenance
        action = body.get("action")
        if action not in {"restart", "update"}:
            raise HTTPException(422, "action must be restart or update")
        return desktop_maintenance.request(ORG, AGENT, "org", "prueba del ciclo de vida (#25)", action=action)

    api.app.add_api_route("/api/fixture/maintenance", post_maintenance, methods=["POST"])
    api.app.router.routes.insert(0, api.app.router.routes.pop())


def _install_fake_conversion() -> None:
    """La conversión de la primera ejecución, simulada en el punto donde el
    motor real la hace (``start_for_engine``, llamado por ``launch.main``)."""
    spec = os.environ.get("ORGTREE_FIXTURE_CONVERT", "")
    if not spec:
        return
    from engine import pg_process
    original = pg_process.start_for_engine
    steps = int(spec) if spec.isdigit() else 2
    gap = float(os.environ.get("ORGTREE_FIXTURE_CONVERT_GAP", "2.5"))

    def converting(root, env, migrator=None, progress=None):
        for step in range(1, steps + 1):
            if progress:
                progress(f"database-convert: copiando la org {step} de {steps}")
            time.sleep(gap)
        if spec == "fail":
            raise pg_process.ConversionFailed(
                f"La conversión de prueba se detuvo en la org {steps}. El registro está en {root / 'conversion'}.")
        return original(root, env, migrator=migrator, progress=progress)

    pg_process.start_for_engine = converting


def _live_frames() -> None:
    from orgtree import supervisor
    time.sleep(8)  # supervisor.stream se conecta al hub del WebSocket al arrancar
    n = 0
    while True:
        n += 1
        supervisor.stream(ORG, AGENT, {"kind": "delta", "text": f"latido {n} "})
        time.sleep(1.5)


original_load_app = launch.load_app


def seeded_load_app():
    result = original_load_app()
    _stub_providers()
    _install_turn_state()
    _install_attention()
    _install_docket()
    _install_maintenance()
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
    _install_fake_conversion()
    if os.name == "nt":
        launch.main()
    else:
        serve_without_lifetime()
