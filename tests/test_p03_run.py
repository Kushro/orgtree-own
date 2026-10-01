"""Windows process controls for the tracked P03 queue and launch wrapper."""
from __future__ import annotations

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout
import child_python
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(os.name == "nt", "Windows job objects and PowerShell")
class P03Run(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="orgtree p03 "))
        self.lock = self.root / "locks"
        self.script = self.root / "p03-run.ps1"
        source = (ROOT / "tools" / "p03-run.ps1").read_text(encoding="utf-8")
        # Ignore unrelated live baseline processes in this disposable lock
        # namespace. Queue/slots/job/launch code are the actual tracked source.
        left = source.index("function Get-BaselineRuns {")
        right = source.index("function Get-FreeCommitGB {", left)
        source = source[:left] + "function Get-BaselineRuns { @() }\n" + source[right:]
        self.script.write_text(source, encoding="utf-8")
        self.children = []

    def tearDown(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=15)
        shutil.rmtree(self.root)

    @staticmethod
    def literal(value):
        return "'" + str(value).replace("'", "''") + "'"

    def start(self, agent, flags="", command=None):
        text = f"& {self.literal(self.script)} -LockDirectory {self.literal(self.lock)} -Agent {self.literal(agent)} {flags}"
        if command:
            text += " -Run @(" + ",".join(self.literal(arg) for arg in command) + ")"
        text += "; exit $LASTEXITCODE"
        child = subprocess.Popen(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", text],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8", errors="replace")
        self.children.append(child)
        return child

    def finish(self, child, code=0):
        out, err = child.communicate(timeout=45)
        self.assertEqual(child.returncode, code, out + err)
        return out

    def status(self):
        return json.loads(self.finish(self.start("status", "-Status")))

    def test_status_is_json_and_failed_launch_keeps_fifo_place(self):
        self.finish(self.start("first", "-Enqueue -Purpose 'landing'"))
        child = self.start("first", "-Wait", [str(self.root / "missing.exe"), "argument"])
        self.finish(child, 70)
        state = self.status()
        self.assertTrue(all(slot["free"] for slot in state["slots"]))
        self.assertEqual([row["agent"] for row in state["queue"]], ["first"])
        # Retry the same entry; a nonzero exit proves launch occurred and spends it.
        self.finish(self.start("first", "-Wait", child_python.argv("-c", "raise SystemExit(7)")), 7)
        self.assertEqual(self.status()["queue"], [])

    def test_waiters_run_fifo_even_when_second_waiter_arrives_first(self):
        self.finish(self.start("first", "-Enqueue -Purpose 'landing'"))
        self.finish(self.start("second", "-Enqueue -Purpose 'landing'"))
        log = self.root / "order.txt"
        def command(name):
            return child_python.argv("-c", f"from pathlib import Path; import time; p=Path({str(log)!r}); p.open('a').write({name!r}+'\\n'); time.sleep(0.4)")
        second = self.start("second", "-Wait", command("second"))
        self.finish(self.start("intruder", command=command("intruder")), 75)
        first = self.start("first", "-Wait", command("first"))
        self.finish(first)
        self.finish(second)
        self.assertEqual(log.read_text().splitlines(), ["first", "second"])
        self.assertEqual(self.status()["queue"], [])

    def test_argument_list_preserves_spaces_quotes_backslashes_and_stderr(self):
        output = self.root / "arguments.json"
        args = ["space here", 'literal"quote', "trailing\\", "", "& % literal"]
        code = "import json,sys; from pathlib import Path; Path(sys.argv[1]).write_text(json.dumps(sys.argv[2:])); sys.stderr.write('child stderr\\n')"
        out = self.finish(self.start("argv", command=child_python.argv("-c", code, str(output), *args)))
        self.assertEqual(json.loads(output.read_text()), args)
        self.assertIn("lock ACQUIRED", out)
        # communicate() already returned the inherited stderr to the caller.
        self.assertIn("child stderr", self.children[-1].communicate()[1])

    def test_killed_wrapper_kills_child_before_lock_becomes_available(self):
        import psutil
        pidfile = self.root / "child.pid"
        code = f"import os,time; from pathlib import Path; Path({str(pidfile)!r}).write_text(str(os.getpid())); time.sleep(90)"
        wrapper = self.start("crash", command=child_python.argv("-c", code))
        deadline = time.monotonic() + 30
        while not pidfile.exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        self.assertTrue(pidfile.exists(), "control child never started")
        childpid = int(pidfile.read_text())
        self.assertTrue(psutil.pid_exists(childpid))
        wrapper.kill()
        wrapper.communicate(timeout=15)
        deadline = time.monotonic() + 5
        while psutil.pid_exists(childpid) and time.monotonic() < deadline:
            time.sleep(0.1)
        self.assertFalse(psutil.pid_exists(childpid), "orphan child survived wrapper death")
        self.assertTrue(all(slot["free"] for slot in self.status()["slots"]))


if __name__ == "__main__":
    unittest.main(verbosity=2)
