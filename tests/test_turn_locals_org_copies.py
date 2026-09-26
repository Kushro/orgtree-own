"""turn-locals: a running turn pins ONE whole-Org copy, not eight.

mem-leak-probe measured (2026-09-26, N=100 on v3 9db3acb) that engine private
bytes track the number of LIVE whole-Org objects, ~80 MB each, and named the
holders as the turn worker's own locals: `_run_turn`'s current_org/_loop_tx
and `_run_one_turn_recorded`'s admission_org, _g_org, _cmp_tx/_adm_tx,
o2/usage_org/_inf_tx. Each was assigned before the turn-output loop and
kept for the whole turn, so every in-flight turn pinned ~8 org versions.

This drives a REAL turn (`_run_turn`, the worker entry) against a fake CLI
that blocks after reading its prompt, and while it is blocked walks the turn
thread's stack frames: the distinct Org objects their locals reach (an Org
itself, or a transaction's `.org`) must number at most ONE — the admission
copy `org`, which the turn genuinely reads throughout.
"""
import contextlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

_ROOT = tempfile.TemporaryDirectory(prefix="orgtree-turn-locals-",
                                    ignore_cleanup_errors=True)
os.environ["ORGTREE_DATA"] = _ROOT.name
os.environ["ORGTREE_WARM"] = "0"
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "engine" / "backend"))

import import_provenance  # noqa: F401,E402

from orgtree import ledger, store, supervisor as sup, warmpool  # noqa: E402

# the fake CLI: init, read the prompt, say so, then block until released.
# mode "boundary": answer the first prompt only once the test says go (after
# it queued a follow-up), so the runner feeds that follow-up at the result
# boundary; then block on the SECOND prompt.
_CHILD = r'''
import json,os,sys,time
flag,mode=sys.argv[1],sys.argv[2]
def emit(e):print(json.dumps(e),flush=True)
def wait(suffix):
    t0=time.time()
    while not os.path.exists(flag+suffix) and time.time()-t0<60:
        time.sleep(0.05)
def answer():
    emit({'type':'assistant','message':{'id':'a','role':'assistant','model':'claude',
          'content':[{'type':'text','text':'done'}],'usage':{'output_tokens':1}}})
    emit({'type':'result','subtype':'success','is_error':False,'result':'done',
          'total_cost_usd':0,'usage':{'output_tokens':1}})
emit({'type':'system','subtype':'init','session_id':'fixture-session','tools':[]})
sys.stdin.readline()
if mode=='boundary':
    open(flag+'.first','w').close()
    wait('.go')
    answer()
    sys.stdin.readline()
open(flag+'.blocked','w').close()
wait('.release')
answer()
'''


def orgs_pinned_by(thread: threading.Thread) -> dict[int, list[str]]:
    """Distinct Org objects the thread's frame locals reach, with who holds
    each (`function:local`)."""
    frame = sys._current_frames().get(thread.ident)
    held: dict[int, list[str]] = {}
    while frame is not None:
        for name, value in list(frame.f_locals.items()):
            org = value if isinstance(value, ledger.Org) else getattr(value, "org", None)
            if isinstance(org, ledger.Org):
                held.setdefault(id(org), []).append(f"{frame.f_code.co_name}:{name}")
        frame = frame.f_back
    return held


class TurnLocals(unittest.TestCase):

    def setUp(self):
        TurnLocals.seq = getattr(TurnLocals, "seq", 0) + 1
        self.slug = f"turn-locals-{TurnLocals.seq}"
        self.nid = "worker"
        self.dir = Path(_ROOT.name) / self.slug
        self.dir.mkdir(exist_ok=True)
        self.script = self.dir / "child.py"
        self.script.write_text(_CHILD, encoding="utf-8")
        self.flag = str(self.dir / "turn")
        org = store.create_org(self.slug)
        org.hire(ledger.USER, None, "opus", 0, self.nid)
        org.node(self.nid)["session_id"] = "fixture-session"
        store.save_org(org)
        Path(sup.scratch_dir(self.slug, self.nid)).mkdir(parents=True, exist_ok=True)
        self.st = sup.state(self.slug, self.nid)
        self.st["busy"] = True
        self.procs = []
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.addCleanup(lambda: store._POOL.close_all(self.slug))
        e = self.stack.enter_context
        e(patch.object(sup, "spawn_env", return_value={
            k: v for k, v in os.environ.items()
            if k.upper() in ("SYSTEMROOT", "WINDIR", "PATH", "TEMP", "TMP")}))
        e(patch.object(sup, "_leash"))
        e(patch.object(sup, "_mcp_infrastructure_fingerprint", return_value="fixture"))
        e(patch.object(sup, "_record_prompt_view"))
        e(patch.object(sup, "cli_diagnosis", return_value=None))
        e(patch.object(sup, "notify"))
        e(patch.object(warmpool, "poke"))
        e(patch.object(warmpool, "warm_decision", return_value=(False, False)))
        e(patch.object(warmpool, "eligible", return_value=(False, "fixture")))
        e(patch.object(sup.appsettings, "wait_for_mcp_tools_enabled", return_value=False))
        e(patch("orgtree.transcript_ingest.capture_safely"))
        self.mode = "plain"
        e(patch.object(sup, "_build_cmd", side_effect=lambda *a, **k: [
            sys.executable, str(self.script), self.flag, self.mode]))
        popen = subprocess.Popen

        def spawn(*args, **kwargs):
            p = popen(*args, **kwargs)
            if kwargs.get("stdin") == subprocess.PIPE:
                self.procs.append(p)
            return p
        e(patch.object(subprocess, "Popen", side_effect=spawn))

    def tearDown(self):
        Path(self.flag + ".release").touch()
        for proc in self.procs:
            try:
                proc.wait(timeout=10)
            except Exception:                                # noqa: BLE001
                proc.kill()
                proc.wait(timeout=5)
            for stream in (proc.stdin, proc.stdout, proc.stderr):
                if stream is not None and not stream.closed:
                    stream.close()

    def test_a_blocked_turn_pins_at_most_one_org(self):
        thread = threading.Thread(
            target=lambda: sup._run_turn(self.slug, self.nid, "hello"),
            daemon=True)
        thread.start()
        deadline = time.time() + 60
        while not os.path.exists(self.flag + ".blocked"):
            self.assertTrue(thread.is_alive(), "the turn ended before the CLI blocked")
            self.assertLess(time.time(), deadline, "the fake CLI never got its prompt")
            time.sleep(0.05)
        time.sleep(0.3)             # the runner is now waiting on the CLI's output
        held = orgs_pinned_by(thread)
        # control: the walk must see the turn's own admission copy, or it
        # proves nothing
        self.assertTrue(any("org" == h.split(":")[1] for hs in held.values() for h in hs),
                        f"the frame walk never saw the turn's `org`: {held}")
        self.assertLessEqual(len(held), 1,
                             f"a blocked turn pins {len(held)} Org copies: {held}")
        Path(self.flag + ".release").touch()
        thread.join(60)
        self.assertFalse(thread.is_alive(), "the turn runner must settle")

    def test_a_turn_fed_a_follow_up_at_the_boundary_pins_at_most_one_org(self):
        """The boundary feed's own transaction (`_bnd_tx`, `o2`,
        `nusage_org`) must not ride the rest of the turn either."""
        self.mode = "boundary"
        thread = threading.Thread(
            target=lambda: sup._run_turn(self.slug, self.nid, "hello"),
            daemon=True)
        thread.start()
        self.wait_for(".first", thread)
        with sup._state_lock:
            self.st.setdefault("queue", []).append("a follow-up")
        Path(self.flag + ".go").touch()
        self.wait_for(".blocked", thread)
        time.sleep(0.3)
        held = orgs_pinned_by(thread)
        self.assertTrue(any("org" == h.split(":")[1] for hs in held.values() for h in hs),
                        f"the frame walk never saw the turn's `org`: {held}")
        self.assertLessEqual(len(held), 1,
                             f"a turn fed at the boundary pins {len(held)} Org copies: {held}")
        Path(self.flag + ".release").touch()
        thread.join(60)
        self.assertFalse(thread.is_alive(), "the turn runner must settle")

    def wait_for(self, suffix, thread):
        deadline = time.time() + 60
        while not os.path.exists(self.flag + suffix):
            self.assertTrue(thread.is_alive(), f"the turn ended before {suffix}")
            self.assertLess(time.time(), deadline, f"never reached {suffix}")
            time.sleep(0.05)


if __name__ == "__main__":
    unittest.main()
