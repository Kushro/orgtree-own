"""Agent read defaults and flag dismissal regressions, without a database."""
import copy
import io
import json
import sys
import unittest
import urllib.error
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock, patch

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from assert_repo_import import assert_repo_import
assert_repo_import(Path(__file__).resolve().parents[1])

from orgtree import api, ledger, mcptool, workdetail


class GetQuickWins(unittest.TestCase):
    def setUp(self):
        self.org = ledger.Org.__new__(ledger.Org)
        self.view = {"slug": "small", "rev": 1, "title": "Small", "status": "review",
                     "objective": "Current specification", "acceptance": [],
                     "history": [{"op": "review_changes", "at": "2026-10-01T10:00:00Z",
                                  "by": {"node": "reviewer"}, "note": "Fix the edge"}],
                     "scope_archive": [{"kind": "decision", "seq": 1, "text": "First ruling"}],
                     "scope": [{"kind": "objective", "before": "Old text", "after": None},
                               {"kind": "decision", "seq": 3, "text": "Second ruling"}],
                     "effective_attention": True}

    def read(self, **args):
        org = MagicMock()
        org.work_get.side_effect = lambda *a, **kw: self.org._work_project(
            self.view, kw["projection"], ledger.Org._work_fields_arg(kw.get("fields")), "get")
        with patch.object(workdetail, "get", return_value=None), \
             patch.object(api.store, "cached_org", return_value=org), \
             patch.object(api, "_work_identity_guard"):
            return api._work_read_call(SimpleNamespace(org="fixture", node="owner"),
                                       {"action": "get", "slug": "small", **args})["item"]

    def test_agent_default_is_compact_and_full_is_explicit(self):
        compact = self.read()
        full = self.read(projection="full")
        self.assertEqual(compact["projection"], "compact")
        self.assertNotIn("history", compact)
        self.assertEqual(compact["latest_verdict"]["note"], "Fix the edge")
        self.assertEqual(full["history"], self.view["history"])
        self.assertNotIn("latest_verdict", full)
        self.assertEqual(set(full), set(self.view) | {"projection", "ref"})

    def test_postgres_agent_door_gets_the_same_default(self):
        with patch.object(workdetail, "get", return_value={"slug": "small"}) as get:
            api._work_read_call(SimpleNamespace(org="fixture", node="owner"),
                                {"action": "get", "slug": "small"})
        self.assertEqual(get.call_args.kwargs["projection"], "compact")

    def test_scope_decisions_omits_spec_rewrites_and_keeps_archived_rulings(self):
        view = self.read(projection="scope_decisions")
        self.assertEqual(view["objective"], "Current specification")
        self.assertEqual([r["seq"] for r in view["decisions"]], [1, 3])
        self.assertNotIn("scope", view)
        self.assertNotIn("history", view)
        self.assertIn("projection=full", view["omissions_how"])

    def test_read_aliases_return_canonical_fields(self):
        view = self.read(fields=["description", "attention", "review", "decisions"])
        self.assertEqual(view["objective"], self.view["objective"])
        self.assertTrue(view["effective_attention"])
        self.assertEqual(view["latest_verdict"]["decision"], "changes")
        self.assertEqual(len(view["decisions"]), 2)
        self.assertNotIn("description", view)

    def test_newest_candidate_verdict_beats_an_older_review(self):
        verdict = {"at": "2026-10-01T11:00:00Z", "candidate": "a" * 40,
                   "decision": "approve", "note": "Exact commit checked"}
        self.view["candidate_verdicts"] = [verdict]
        self.assertEqual(self.read()["latest_verdict"], verdict)
        self.view["history"].append({"op": "review_changes", "at": "2026-10-01T12:00:00Z",
                                      "note": "A newer sendback"})
        self.assertEqual(self.read()["latest_verdict"]["note"], "A newer sendback")

    def test_review_approval_keeps_its_acceptance_note(self):
        self.view["accepted"] = {"at": "2026-10-01T12:00:00Z", "via": "review_approve",
                                  "note": "Approved with limits"}
        self.view["history"].append({"op": "review_approve", "at": "2026-10-01T12:00:01Z"})
        self.assertEqual(self.read()["latest_verdict"]["note"], "Approved with limits")

    def test_postgres_projection_context_uses_the_same_policy(self):
        ctx = workdetail.Context.__new__(workdetail.Context)
        for projection in ("compact", "full", "scope_decisions"):
            self.assertEqual(ctx._work_project(self.view, projection, None, "get"),
                             self.org._work_project(self.view, projection, None, "get"))

    def test_valid_field_refusal_survives_the_mcp_transport_whole(self):
        with self.assertRaises(api.HTTPException) as caught:
            self.read(fields=["unknown"])
        detail = caught.exception.detail
        for field in ("description", "attention", "review", "decisions", "latest_verdict"):
            self.assertIn(field, detail)
        payload = json.dumps({"detail": detail + " padding" * 100})
        error = urllib.error.HTTPError("http://fixture", 422, "bad field", {},
                                       io.BytesIO(payload.encode()))
        with patch.object(mcptool.urllib.request, "urlopen", side_effect=error):
            kind, text = mcptool._post({"tool": "orgtree_work"})
        self.assertEqual(kind, "refused")
        self.assertEqual(text, payload)


class DismissQuickWins(unittest.TestCase):
    def test_unfinished_work_is_still_blocked(self):
        org = ledger.Org.__new__(ledger.Org)
        org.d = {"nodes": {}}
        item = {"slug": "small", "status": "in_progress", "rev": 1,
                "manual_attention": {"set_rev": 1, "reason": "Confirm"}}
        with patch.object(org, "_work_find", return_value=(item, False)), \
             patch.object(org, "_work_questions", return_value=[]), \
             patch.object(org, "_log"):
            result = org.work_dismiss_attention("small", 1)
        self.assertEqual(result["status"], "blocked")
        self.assertIn("Confirm", item["blocked_reason"])

    def test_done_review_and_archived_done_keep_their_state(self):
        for status, archived in (("done", False), ("review", False), ("done", True)):
            with self.subTest(status=status, archived=archived):
                org = ledger.Org.__new__(ledger.Org)
                org.d = {"nodes": {}}
                item = {"slug": "small", "rev": 3, "status": status,
                        "status_at": "old", "accepted": {"note": "Checked"},
                        "reviewer": {"node": "reviewer"}, "review_packet": {"candidate": "a" * 40},
                        "blocked_reason": None, "archived_at": "old" if archived else None,
                        "manual_attention": {"set_rev": 1, "reason": "Please confirm"},
                        "history": []}
                before = copy.deepcopy(item)
                with patch.object(org, "_work_find", return_value=(item, archived)), \
                     patch.object(org, "_work_questions", return_value=[]), \
                     patch.object(org, "_log"), patch.object(org, "_work_archive") as archive:
                    result = org.work_dismiss_attention("small", 1)
                self.assertEqual(result["status"], status)
                for field in ("status", "status_at", "accepted", "reviewer", "review_packet",
                              "blocked_reason", "archived_at"):
                    self.assertEqual(item[field], before[field], field)
                archive.assert_not_called()
                self.assertIsNone(item["manual_attention"])
                self.assertEqual(item["dismissals"][0]["reason"], "Please confirm")
                self.assertEqual(item["history"][-1]["op"], "dismiss_attention")


if __name__ == "__main__":
    unittest.main()
