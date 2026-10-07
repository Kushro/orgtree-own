"""Motor falso para probar el supervisor sin el motor real.

Imita el contrato de arranque de engine/launch.py: lee ORGTREE_DATA y
ORGTREE_V2_TOKEN, escribe checkpoints y la línea ready por stdout y atiende
POST /api/desktop/shutdown con el token. FAKE_ENGINE_MODE elige el caso.
"""

import http.server
import json
import os
import sys
import threading
import time
from pathlib import Path

MODE = os.environ.get("FAKE_ENGINE_MODE", "ready")
ROOT = str(Path(os.environ["ORGTREE_DATA"]).resolve())
TOKEN = os.environ.pop("ORGTREE_V2_TOKEN", "")
PID = os.getpid()


def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def progress(sequence, phase):
    emit({"type": "startup-progress", "protocol": 1, "pid": PID, "sequence": sequence,
          "phase": phase, "dataRootId": ROOT})


stop = threading.Event()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _authorized(self):
        return self.headers.get("X-Orgtree-Desktop-Token", "") == TOKEN

    def do_GET(self):
        if not self._authorized():
            return self._reply(401, {"detail": "missing authentication"})
        self._reply(200, {"protocol": 1, "pid": PID, "dataRootId": ROOT})

    def do_POST(self):
        if not self._authorized():
            return self._reply(401, {"detail": "missing authentication"})
        if self.path == "/api/desktop/shutdown" and MODE != "ignore-shutdown":
            stop.set()
        self._reply(200, {"accepted": True})


if not TOKEN:
    sys.exit("ORGTREE_V2_TOKEN is required")
if "ORGTREE_V2_TOKEN" in os.environ:
    sys.exit("token still in environment")

if MODE == "exit":
    sys.exit(3)
if MODE == "silent":
    time.sleep(60)
    sys.exit(0)
if MODE == "refused":
    emit({"type": "refused", "code": "root-owned", "reason": "another engine owns this data root"})
    sys.exit(1)

# Lo que el supervisor le pasó al motor, para las pruebas del modo empaquetado.
REPORTED = ("ORGTREE_PG_BOOTSTRAP", "ORGTREE_PG_CUSTODIAN", "ORGTREE_P03_PG_BIN", "ORGTREE_V2_UI_DIR",
            "PGPASSWORD", "PGUSER")
(Path(ROOT) / "fake-env.json").write_text(json.dumps({k: os.environ.get(k) for k in REPORTED}), encoding="utf-8")

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
print("texto suelto que el supervisor ignora", flush=True)
if MODE in ("convert", "convert-stall"):
    # La conversión de la primera ejecución: fases `database-convert…` a 0,6 s.
    # Con "convert-stall" el último paso calla más que el plazo de conversión.
    for sequence in range(1, 4):
        progress(sequence, f"database-convert: org {sequence}/3")
        time.sleep(3 if MODE == "convert-stall" and sequence == 3 else 0.6)
    progress(4, "api-loaded")
elif MODE == "convert-fail":
    progress(1, "database-convert: org 1/1")
    emit({"type": "refused", "code": "conversion-failed", "reason": "no se pudo copiar la org 1; ver conversion"})
    sys.exit(1)
elif MODE == "slow":
    # Cada checkpoint llega antes del plazo de silencio, pero el total lo supera.
    for sequence in range(1, 5):
        time.sleep(0.6)
        progress(sequence, "migrate")
else:
    progress(1, "lifetime-owned")
pid = PID + 1 if MODE == "bad-pid" else PID
emit({"type": "ready", "protocol": 1, "port": server.server_address[1], "pid": pid, "dataRootId": ROOT})
stop.wait(120)
server.shutdown()
