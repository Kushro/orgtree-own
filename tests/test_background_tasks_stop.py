"""Retire, dissolve, delete and compaction STOP an agent's background tasks.

User ruling 2026-09-29 (docket the-guard-that-stops-retiring-an-agent-with-back):
when an agent is retired, dissolved, deleted or compacted while background
tasks are still running, Orgtree goes ahead and stops those tasks — no refusal
and no force option. The old `bg_open` refusals could never fire, because
nothing in the product wrote that field.

Here the background task is opened through the REAL product path: the real turn
runner reads a stand-in CLI's `background_tasks_changed` event, and the task is
a real grandchild process. Each op is then called through its real door, and the
test checks that the grandchild, the CLI and the turn are all gone.

Every process and ledger is a local fixture; no provider is contacted.
"""
import contextlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

_ROOT = tempfile.TemporaryDirectory(prefix="orgtree-bg-stop-")
os.environ["ORGTREE_DATA"] = _ROOT.name
os.environ["ORGTREE_WARM"] = "0"
os.environ["ORGTREE_TURNLOG"] = "1"

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout

from orgtree import api, ledger, store, supervisor as sup, warmpool

# The stand-in CLI. It starts ONE background job (a real sleeping process),
# reports it the way Claude Code does, ends its turn with a result, and then —
# like the real CLI — keeps its own process alive, draining, until the job ends.
_CHILD = r'''
import json,os,subprocess,sys
from pathlib import Path
marker=sys.argv[1]
def emit(event):print(json.dumps(event),flush=True)
emit({'type':'system','subtype':'init','session_id':'fixture-session','tools':[]})
sys.stdin.readline()
job=subprocess.Popen([sys.executable,'-c','import time;time.sleep(600)'])
Path(marker+'.job').write_text(str(job.pid))
emit({'type':'system','subtype':'background_tasks_changed','tasks':[{'task_id':'job1','task_type':'local_bash','description':'long job'}]})
emit({'type':'assistant','message':{'id':'a','role':'assistant','content':[{'type':'text','text':'started a long job'}],'usage':{'output_tokens':1}}})
Path(marker).write_text(str(os.getpid()))
emit({'type':'result','subtype':'success','is_error':False,'total_cost_usd':0,'usage':{'output_tokens':1}})
for line in sys.stdin:pass
job.wait()
emit({'type':'system','subtype':'background_tasks_changed','tasks':[]})
'''

_NOTICE_CHILD = r'''
import json,sys
from pathlib import Path
marker=sys.argv[1]
def emit(event):print(json.dumps(event),flush=True)
emit({'type':'system','subtype':'init','session_id':'fixture-session','tools':[]})
sys.stdin.readline()
for event in json.loads(Path(marker+'.events').read_text()):emit(event)
Path(marker).write_text('0')
emit({'type':'result','subtype':'success','is_error':False,'total_cost_usd':0,'usage':{'output_tokens':1}})
for line in sys.stdin:pass
'''

REQUEST = SimpleNamespace(state=SimpleNamespace())
STOPPED = "background task(s) running; they were stopped"


def eventually(predicate, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(.05)
    return bool(predicate())


def alive(pid):
    import psutil
    try:
        return psutil.Process(pid).status() != psutil.STATUS_ZOMBIE
    except psutil.NoSuchProcess:
        return False


class BackgroundTasksStop(unittest.TestCase):
    seq = 0

    def setUp(self):
        self.assertEqual(Path(store.DATA_ROOT), Path(_ROOT.name))
        type(self).seq += 1
        self.slug = f"bg-stop-{self.seq}"
        self.nid = "worker"
        self.dir = Path(_ROOT.name) / f"fixture-{self.seq}"
        self.dir.mkdir()
        self.script = self.dir / "child.py"
        self.script.write_text(_CHILD, encoding="utf-8")
        self.marker = self.dir / "cli.pid"
        org = store.create_org(self.slug)
        org.hire(ledger.USER, None, "opus", 3, "lead")
        org.hire(ledger.USER, "lead", "opus", 0, self.nid)
        org.node(self.nid)["session_id"] = "fixture-session"
        store.save_org(org)
        Path(sup.scratch_dir(self.slug, self.nid)).mkdir(parents=True, exist_ok=True)
        self.st = sup.state(self.slug, self.nid)
        self.st["busy"] = True
        self.thread = None
        self.procs = []
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.stack.enter_context(patch.object(sup, "spawn_env", return_value={
            k: v for k, v in os.environ.items()
            if k.upper() in ("SYSTEMROOT", "WINDIR", "PATH", "TEMP", "TMP")}))
        self.stack.enter_context(patch.object(sup, "_leash"))
        self.stack.enter_context(patch.object(sup, "_mcp_infrastructure_fingerprint", return_value="fixture"))
        self.stack.enter_context(patch.object(sup, "_record_prompt_view"))
        self.stack.enter_context(patch.object(sup, "cli_diagnosis", return_value=None))
        self.stack.enter_context(patch.object(warmpool, "poke"))
        self.stack.enter_context(patch.object(warmpool, "warm_decision", return_value=(False, False)))
        self.stack.enter_context(patch.object(warmpool, "eligible", return_value=(False, "fixture")))
        self.stack.enter_context(patch.object(sup.appsettings, "wait_for_mcp_tools_enabled", return_value=False))
        self.stack.enter_context(patch("orgtree.transcript_ingest.capture_safely"))
        self.stack.enter_context(patch.object(sup, "export_predecessor_transcript"))
        self.stack.enter_context(patch.object(sup, "export_after_commit"))
        popen = subprocess.Popen

        def spawn(*args, **kwargs):
            p = popen(*args, **kwargs)
            if kwargs.get("stdin") == subprocess.PIPE:
                self.procs.append(p)
            return p
        self.stack.enter_context(patch.object(subprocess, "Popen", side_effect=spawn))

    def tearDown(self):
        for path in (Path(str(self.marker) + ".job"), self.marker):
            if path.exists():
                pid = int(path.read_text())
                if os.name == "nt":
                    subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)],
                                   capture_output=True, timeout=5,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
                else:
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(pid, 9)
        for proc in self.procs:
            if proc.poll() is None:
                proc.kill()
            proc.wait(timeout=5)
        if self.thread is not None:
            self.thread.join(10)
            self.assertFalse(self.thread.is_alive(), "fixture teardown must settle")
        for proc in self.procs:
            for stream in (proc.stdin, proc.stdout, proc.stderr):
                if stream is not None and not stream.closed:
                    stream.close()
        store._POOL.close_all(self.slug)

    # ---- helpers

    def start_turn(self):
        cmd = [sys.executable, str(self.script), str(self.marker)]
        self.stack.enter_context(patch.object(sup, "_build_cmd", return_value=cmd))
        self.thread = threading.Thread(target=lambda: sup._run_one_turn(
            self.slug, self.nid, {"cmd": True, "text": "/fixture"}), daemon=True)
        self.thread.start()

    def start(self):
        """A real turn that leaves one background job running: the runner
        does not park a process with live children, so the turn stays busy."""
        self.start_turn()
        self.assertTrue(eventually(self.marker.exists), self.st.get("last_error"))
        self.assertTrue(eventually(lambda: self.st.get("bg_tasks") == 1),
                        "the runner must see the CLI's background task")
        self.job = int(Path(str(self.marker) + ".job").read_text())
        self.assertTrue(alive(self.job))
        self.assertTrue(self.st["busy"])

    def op(self, op, node, **kw):
        return api.org_op(self.slug, api.Op(op=op, node=node, **kw), REQUEST)

    def assert_stopped(self, result):
        warnings = result.get("warnings") or []
        self.assertTrue(any(STOPPED in w and self.nid in w for w in warnings),
                        f"the result must say the tasks were stopped: {warnings}")
        self.assertTrue(eventually(lambda: not alive(self.job)),
                        "the background job must be stopped")
        self.assertTrue(eventually(lambda: self.procs[0].poll() is not None),
                        "the agent's CLI must be ended")
        self.thread.join(10)
        self.assertFalse(self.thread.is_alive(), "the cut turn must reach its finally")
        # the death notice is recorded but must NOT wake the agent: a new turn
        # would restart the agent being archived, or run on a replaced session
        time.sleep(1.5)
        self.assertEqual(len(self.procs), 1, "stopping must not start another turn")

    # ---- the ruled behaviour, one door at a time

    def test_retire_stops_background_tasks(self):
        self.start()
        result = self.op("retire", self.nid)
        self.assert_stopped(result)
        self.assertEqual(store.load_org(self.slug).node(self.nid)["state"], "archived")
        self.assertEqual(self.st["bg_tasks"], 0)

    def test_dissolve_stops_a_descendants_background_tasks(self):
        self.start()
        result = self.op("dissolve", "lead")
        self.assert_stopped(result)
        org = store.load_org(self.slug)
        self.assertEqual(org.node(self.nid)["state"], "archived")
        self.assertEqual(org.node("lead")["state"], "archived")

    def test_delete_stops_background_tasks(self):
        self.start()
        result = self.op("delete", self.nid)
        self.assert_stopped(result)
        self.assertNotIn(self.nid, store.load_org(self.slug).nodes)

    def test_cheap_compact_stops_background_tasks(self):
        self.start()
        result = self.op("cheap_compact", self.nid)
        self.assert_stopped(result)
        node = store.load_org(self.slug).node(self.nid)
        self.assertEqual(node["state"], "live")
        self.assertNotEqual(node["session_id"], "fixture-session")

    def test_cheap_compact_through_the_agent_door_stops_background_tasks(self):
        self.start()
        result = api.agent_call(
            api.AgentCall(org=self.slug, node="lead", tool="orgtree_cheap_compact",
                          args={"node": self.nid}), REQUEST)
        self.assert_stopped(result)
        self.assertNotEqual(store.load_org(self.slug).node(self.nid)["session_id"],
                            "fixture-session")

    def test_manual_compaction_stops_background_tasks(self):
        with store.DOC_LOCK:
            org = store.load_org(self.slug)
            org.node(self.nid)["occupancy"] = 50000
            store.save_org(org)
        started = threading.Event()
        self.stack.enter_context(patch.object(
            sup, "manual_compact", side_effect=lambda *a: started.set()))
        self.start()
        result = api.node_compact(self.slug, self.nid)
        self.assertTrue(result["started"])
        self.assert_stopped(result)
        self.assertTrue(started.wait(10), "the compaction must still run")

    def test_compact_command_in_chat_stops_background_tasks(self):
        # the /compact chat command is a second route to the same compaction,
        # with its own copy of the stop; arguments add a warning, never
        # replace the stop's
        with store.DOC_LOCK:
            org = store.load_org(self.slug)
            org.node(self.nid)["occupancy"] = 50000
            store.save_org(org)
        started = threading.Event()
        self.stack.enter_context(patch.object(
            sup, "manual_compact", side_effect=lambda *a: started.set()))
        self.start()
        result = api.node_message(self.slug, self.nid, api.Message(text="/compact now"))
        self.assertEqual((result.get("accepted"), result.get("compacting")), (True, True))
        self.assert_stopped(result)
        self.assertTrue(any("arguments are ignored" in w for w in result["warnings"]))
        self.assertEqual(self.st["bg_tasks"], 0)
        self.assertTrue(started.wait(10), "the compaction must still run")

    def test_a_failed_stop_leaves_real_task_deaths_waking(self):
        self.st["bg_tasks"] = 1
        with patch("orgtree.halt.cut_for_archive", side_effect=OSError("no")):
            warning = sup.stop_background(self.slug, store.load_org(self.slug),
                                          self.nid, subtree=False)
        self.assertTrue(any("stopping them failed" in w for w in warning or []), warning)
        self.assertNotIn("bg_stop_requested", self.st)

    def test_a_bulk_if_idle_compaction_leaves_a_busy_agent_alone(self):
        # the bulk run's standing rule is unchanged: a busy agent is skipped,
        # never interrupted — so its background task keeps running
        self.start()
        with self.assertRaises(api.HTTPException) as raised:
            self.op("cheap_compact", self.nid, if_idle=True)
        self.assertEqual(raised.exception.status_code, 409)
        self.assertTrue(alive(self.job))
        self.assertEqual(self.st["bg_tasks"], 1)

    def notices(self, events):
        self.script.write_text(_NOTICE_CHILD, encoding='utf-8')
        Path(str(self.marker) + '.events').write_text(json.dumps(events), encoding='utf-8')
        wake = self.stack.enter_context(patch.object(sup, 'send_message'))
        self.start_turn()
        self.thread.join(15)
        self.assertFalse(self.thread.is_alive(), 'the notification fixture must finish')
        self.assertTrue(self.marker.exists(), 'the stand-in must actually emit every event')
        # This fixture has no child process; do not hand teardown a fake PID.
        self.marker.unlink()
        mails = store.load_org(self.slug).d.get('mail', {}).get(self.nid, [])
        return [m for m in mails if 'BACKGROUND TASK STOPPED' in m.get('body', '')], wake

    @staticmethod
    def task_events(status='failed', **extra):
        return [
            {'type': 'system', 'subtype': 'background_tasks_changed', 'tasks': [
                {'task_id': 'job1', 'tool_use_id': 'bash1', 'description': 'test job'}]},
            {'type': 'system', 'subtype': 'background_tasks_changed', 'tasks': []},
            {'type': 'system', 'subtype': 'task_notification', 'task_id': 'job1',
             'tool_use_id': 'bash1', 'status': status, **extra}]

    @staticmethod
    def tool_result(tool='bash1', content='exit code 1: no matches'):
        return {'type': 'user', 'message': {'content': [
            {'type': 'tool_result', 'tool_use_id': tool, 'content': content}]}}

    def test_foreground_nonzero_notification_sends_no_stop_notice(self):
        notify = self.task_events(exit_code=75)[-1]
        mails, wake = self.notices([self.tool_result(), notify])
        self.assertEqual(mails, [])
        wake.assert_not_called()

    def test_returned_background_output_sends_no_stop_notice(self):
        events = self.task_events(exit_code=1)
        events.insert(1, self.tool_result())
        mails, wake = self.notices(events)
        self.assertEqual(mails, [])
        wake.assert_not_called()

    def test_launch_ack_does_not_hide_a_real_stop_and_duplicates_mail_once(self):
        events = self.task_events(status='stopped', exit_code=-9)
        events.insert(1, self.tool_result(content='Command running in background with ID: job1'))
        events.append(events[-1].copy())
        mails, wake = self.notices(events)
        self.assertEqual(len(mails), 1)
        self.assertIn('status: stopped; exit code: -9', mails[0]['body'])
        self.assertNotIn('nothing killed it', mails[0]['body'])
        wake.assert_called_once()

    def test_completed_and_unknown_notifications_do_not_claim_a_stop(self):
        events = self.task_events(status='completed')
        events.append(dict(events[-1], status='running'))
        events.append(dict(events[-1], status='stopped'))
        mails, wake = self.notices(events)
        self.assertEqual(mails, [])
        wake.assert_not_called()

    def test_normal_background_nonzero_exit_does_not_claim_a_stop(self):
        events = self.task_events(exit_code=75)
        events.append(dict(events[-1], status='stopped'))
        mails, wake = self.notices(events)
        self.assertEqual(mails, [])
        wake.assert_not_called()

    def test_real_stop_keeps_zero_exit_code(self):
        mails, _ = self.notices(self.task_events(status='stopped', exit_code=0))
        self.assertEqual(len(mails), 1)
        self.assertIn('exit code: 0', mails[0]['body'])

    def test_real_stop_uses_summary_exit_code_or_says_unavailable(self):
        from orgtree.background_notices import stop_summary
        self.assertIn('exit code: 75', stop_summary({'status': 'failed', 'summary': 'exit code 75'}))
        mails, _ = self.notices(self.task_events(status='stopped'))
        self.assertEqual(len(mails), 1)
        self.assertIn('exit code: unavailable (not reported by CLI)', mails[0]['body'])

    def test_agent_requested_stop_does_not_wake_it_again(self):
        events = self.task_events(status='stopped')
        events.insert(1, {'type': 'assistant', 'message': {'content': [
            {'type': 'tool_use', 'id': 'stop1', 'name': 'TaskStop', 'input': {'task_id': 'job1'}}]}})
        mails, wake = self.notices(events)
        self.assertEqual(mails, [])
        wake.assert_not_called()

    def test_task_output_result_consumes_the_original_job(self):
        events = self.task_events()
        events[1:1] = [
            {'type': 'assistant', 'message': {'content': [
                {'type': 'tool_use', 'id': 'read1', 'name': 'TaskOutput', 'input': {'task_id': 'job1'}}]}},
            self.tool_result('read1', '<status>failed</status>exit code 1')]
        mails, wake = self.notices(events)
        self.assertEqual(mails, [])
        wake.assert_not_called()

    def test_task_output_timeout_does_not_hide_a_later_stop(self):
        events = self.task_events(status='stopped')
        events[1:1] = [
            {'type': 'assistant', 'message': {'content': [
                {'type': 'tool_use', 'id': 'read1', 'name': 'TaskOutput', 'input': {'task_id': 'job1'}}]}},
            self.tool_result('read1', '<retrieval_status>timeout</retrieval_status>')]
        mails, wake = self.notices(events)
        self.assertEqual(len(mails), 1)
        wake.assert_called_once()


if __name__ == "__main__":
    unittest.main()
