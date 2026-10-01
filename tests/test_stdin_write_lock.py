"""One write lock per Claude process's stdin.

Item harden-live-effort-delivery-write-lock-park-race (scope N3). The turn
thread writes the prompt and every boundary feed to a Claude process's stdin,
while the request thread can write an interrupt or a live effort change to the
same pipe. A text-mode write of a long line is not atomic, so two writers
could interleave and the CLI would read two broken lines. These tests pin:

  §1  concurrent `_stdin_send` calls on one process never interleave: every
      line read back is whole JSON (a slow fake pipe that writes in small
      chunks makes an unlocked write interleave reliably)
  §2  two different processes do not share a lock
  §3  `send_live_effort` waits for the process's lock, and a lock that stays
      held answers "next turn" instead of writing or hanging
  §4  `interrupt_turn` waits for the same lock, and reports failure the same way
  §5  no Claude stdin writer in supervisor.py bypasses `_stdin_send`

Every process here is a fake; no provider is contacted.
"""
import json
import os
from pathlib import Path
import re
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

_ROOT = tempfile.TemporaryDirectory(prefix="orgtree-stdin-lock-")
os.environ["ORGTREE_DATA"] = _ROOT.name
os.environ["ORGTREE_WARM"] = "0"

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout

from orgtree import ledger, store, supervisor as sup


class SlowPipe:
    """A stdin that takes each write in 8-character chunks with a pause in
    between, the way a long write can be split under contention."""

    def __init__(self):
        self.chunks = []
        self.closed = False

    def write(self, text):
        for i in range(0, len(text), 8):
            self.chunks.append(text[i:i + 8])
            time.sleep(0.0002)
        return len(text)

    def flush(self):
        pass

    def lines(self):
        return [ln for ln in "".join(self.chunks).split("\n") if ln]


class FakeProc:
    def __init__(self):
        self.stdin = SlowPipe()

    def poll(self):
        return None


def _hammer(proc, writers=4, each=15):
    def run(k):
        for i in range(each):
            sup._stdin_send(proc, json.dumps({"writer": k, "i": i,
                                              "pad": "x" * 200}) + "\n")
    threads = [threading.Thread(target=run, args=(k,)) for k in range(writers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(30)
    return threads


class StdinSendTests(unittest.TestCase):
    def test_concurrent_writers_never_interleave(self):
        proc = FakeProc()
        threads = _hammer(proc)
        self.assertFalse(any(t.is_alive() for t in threads))
        lines = proc.stdin.lines()
        self.assertEqual(len(lines), 4 * 15)
        for line in lines:
            json.loads(line)   # a torn line raises here

    def test_each_process_has_its_own_lock(self):
        a, b = FakeProc(), FakeProc()
        self.assertIs(sup._stdin_lock(a), sup._stdin_lock(a))
        self.assertIsNot(sup._stdin_lock(a), sup._stdin_lock(b))

    def test_a_bounded_wait_raises_oserror_while_the_lock_is_held(self):
        proc = FakeProc()
        lock = sup._stdin_lock(proc)
        with lock:
            with self.assertRaises(OSError):
                sup._stdin_send(proc, "{}\n", wait=0.05)
        self.assertEqual(proc.stdin.chunks, [])


class RequestThreadWriterTests(unittest.TestCase):
    seq = 0

    def setUp(self):
        type(self).seq += 1
        self.slug = f"stdin-lock-{self.seq}"
        self.nid = "worker"
        org = store.create_org(self.slug)
        org.hire(ledger.USER, None, "opus", 0, self.nid)
        store.save_org(org)
        self.org = store.load_org(self.slug)
        self.proc = FakeProc()
        self.st = sup.state(self.slug, self.nid)
        with sup._state_lock:
            self.st["proc"] = self.proc
            self.st["responding"] = True
            self.st["busy"] = True
        self.addCleanup(store._POOL.close_all, self.slug)

    def tearDown(self):
        with sup._state_lock:
            for k in ("proc", "responding", "busy", "interrupted",
                      sup._LIVE_EFFORT_KEY):
                self.st.pop(k, None)

    def test_live_effort_waits_for_a_turn_write_in_progress(self):
        lock = sup._stdin_lock(self.proc)
        lock.acquire()
        out = {}
        t = threading.Thread(target=lambda: out.update(
            sup.send_live_effort(self.org, self.nid, previous="low")))
        t.start()
        time.sleep(0.2)
        self.assertEqual(self.proc.stdin.chunks, [],
                         "the effort line was written while the lock was held")
        lock.release()
        t.join(5)
        self.assertEqual(out.get("delivery"), "sent")
        (line,) = self.proc.stdin.lines()
        self.assertEqual(json.loads(line)["request"]["subtype"],
                         "apply_flag_settings")

    def test_live_effort_is_next_turn_when_the_lock_stays_held(self):
        with patch.object(sup, "STDIN_LOCK_WAIT_S", 0.05):
            with sup._stdin_lock(self.proc):
                out = sup.send_live_effort(self.org, self.nid, previous="low")
        self.assertEqual(out["delivery"], "next_turn")
        self.assertEqual(self.proc.stdin.chunks, [])
        self.assertNotIn(sup._LIVE_EFFORT_KEY, self.st)

    def test_interrupt_waits_for_the_same_lock(self):
        with patch.object(sup, "STDIN_LOCK_WAIT_S", 0.05):
            with sup._stdin_lock(self.proc):
                out = sup.interrupt_turn(self.slug, self.nid)
        self.assertIs(out["interrupted"], False, out)
        self.assertIn("did not finish", out.get("reason", ""))
        self.assertEqual(self.proc.stdin.chunks, [])


class NoBypassTests(unittest.TestCase):
    def test_no_raw_stdin_write_is_left_in_supervisor(self):
        src = Path(sup.__file__).read_text(encoding="utf-8")
        code = "\n".join(ln for ln in src.splitlines()
                         if not ln.lstrip().startswith("#"))
        # the helper's own write and flush are the only raw ones left
        helper = code.index("def _stdin_send(")
        helper_end = code.index("\ndef ", helper + 1)
        rest = code[:helper] + code[helper_end:]
        raw = re.findall(r"\bproc\.stdin\.(?:write|flush)\(", rest)
        self.assertEqual(raw, [], "every Claude stdin write goes through "
                                  "_stdin_send, so it takes the process lock")
        # the three turn-thread feeds and the two request-thread writers
        self.assertEqual(len(re.findall(r"_stdin_send\(proc,", code)), 5)


if __name__ == "__main__":
    unittest.main()
