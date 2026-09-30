"""Write every thread's stack to disk when the engine stops answering.

On 2026-09-30 the live engine stopped answering every request for ten minutes
while its process stayed alive (item v3-orgtree-froze-and-crashed-around-09-40-09-50z),
and it left no trace of WHERE it was stuck: the engine keeps no log of its own,
and the hung process was killed before anyone could look. This watch makes the
next one leave that trace.

A daemon thread asks the engine's own liveness route (``/api/desktop/alive``), over real HTTP, every
PROBE_INTERVAL. That route is a plain ``def``, so an answer needs the event
loop AND a free worker thread: it fails on a blocked loop and on a starved
thread pool alike. Before each probe it arms ``faulthandler.dump_traceback_later``,
whose timer is a C thread that needs no GIL, so the dump is written even when
the stall is a thread holding the GIL. A probe that answers in time cancels the
timer and its header is truncated away, so the file only grows on a stall.

The file is ``<data>/diagnostics/engine-stall-stacks.txt`` and rotates to
``.1`` at start once it passes MAX_BYTES. The service host's liveness watch
(engine/service_host.py) is what ENDS a hang; this only explains it.
"""

from __future__ import annotations

import faulthandler
import os
from pathlib import Path
import threading
import time
from typing import Callable
import urllib.error
import urllib.request

PROBE_INTERVAL = 30.0
STALL_AFTER = 60.0
MAX_BYTES = 8 * 1024 * 1024
DUMP = Path("diagnostics") / "engine-stall-stacks.txt"


def alive_probe(port: int, token: str, timeout: float) -> Callable[[], bool]:
    def probe() -> bool:
        request = urllib.request.Request(f"http://127.0.0.1:{port}/api/desktop/alive",
                                         headers={"X-Orgtree-Desktop-Token": token})
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                response.read()
                return response.status == 200
        except (urllib.error.URLError, OSError, ValueError):
            return False
    return probe


class StallWatch:
    def __init__(self, path: Path, probe: Callable[[], bool], *,
                 interval: float = PROBE_INTERVAL, stall_after: float = STALL_AFTER) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        try:
            if path.stat().st_size > MAX_BYTES:
                os.replace(path, path.with_name(path.name + ".1"))
        except OSError:
            pass
        self.path, self._probe = path, probe
        self.interval, self.stall_after = interval, stall_after
        # Unbuffered append: faulthandler writes to the descriptor directly.
        self._stream = path.open("ab+", buffering=0)
        self._stop = threading.Event()

    def start(self) -> None:
        threading.Thread(target=self._run, daemon=True, name="engine-stall-watch").start()

    def stop(self) -> None:
        self._stop.set()

    def _run(self) -> None:
        while not self._stop.wait(self.interval):
            self.check_once()

    def check_once(self) -> bool:
        """One probe. True when it answered in time (nothing kept)."""
        start = self._stream.seek(0, os.SEEK_END)
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        self._stream.write((f"\n=== {stamp} pid {os.getpid()}: the engine did not answer its liveness "
                            f"route within {self.stall_after:g}s; every thread's stack follows ===\n").encode())
        began = time.monotonic()
        faulthandler.dump_traceback_later(self.stall_after, repeat=False, file=self._stream)
        try:
            answered = self._probe()
        finally:
            faulthandler.cancel_dump_traceback_later()
        elapsed = time.monotonic() - began
        if elapsed < self.stall_after:
            # Answered (or refused) in time: no dump was written, drop the header.
            self._stream.seek(start)
            self._stream.truncate()
            return answered
        self._stream.write(f"=== the probe {'answered' if answered else 'failed'} after {elapsed:.1f}s ===\n".encode())
        return False


def start_stall_watch(data: Path, port: int, token: str) -> StallWatch | None:
    """Best effort: a watch that cannot open its file must not stop the engine."""
    try:
        watch = StallWatch(data / DUMP, alive_probe(port, token, STALL_AFTER + 60.0))
    except OSError:
        return None
    watch.start()
    return watch
