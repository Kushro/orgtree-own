"""Light docket consumers, scoped invalidation, deltas and retained full detail."""
from __future__ import annotations
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

_temp = tempfile.TemporaryDirectory(prefix="work-ui-", ignore_cleanup_errors=True)
(Path(_temp.name) / "data").mkdir()
os.environ.update(ORGTREE_DATA=str(Path(_temp.name) / "data"),
                  ORGTREE_STORE_BACKEND="sqlite", ORGTREE_V2_TOKEN="operator")
for _key in ("ORGTREE_V1_ROOT", "ORGTREE_V1_DATA_ROOT", "ORGTREE_V2_PORT"):
    os.environ.pop(_key, None)
import import_provenance  # noqa: E402,F401
from engine.launch import load_app  # noqa: E402
app, *_ = load_app()
from fastapi.testclient import TestClient  # noqa: E402
from orgtree import ledger, store, work_ui  # noqa: E402

OP = {"X-Orgtree-Desktop-Token": "operator"}


class WorkUI(unittest.TestCase):
    def setUp(self):
        self.slug = "ui-" + os.urandom(4).hex()
        org = store.create_org(self.slug)
        org.hire(ledger.USER, None, "luna", 0, "boss")
        org.work_create("boss", title="Visible", objective="Searchable full description", owner="boss")
        org.work_create("boss", title="Hidden", objective="Backlog description", owner="boss")
        visible, hidden = org.d["work_items"]
        visible["evidence"] = [{"kind": "note", "note": "heavy evidence " * 10000}]
        hidden["status"] = "backlogged"
        hidden["manual_attention"] = {"reason": "Please decide", "set_rev": 1,
                                       "at": "2026-01-01", "by": {"node": "boss"}}
        store.save_org(org)
        self.client = TestClient(app)
        self.url = f"/api/orgs/{self.slug}/work-items-view"

    def get(self, etag=None, query=""):
        return self.client.get(self.url + query, headers={**OP, **({"If-None-Match": etag} if etag else {})})

    def edit(self, fn):
        org = store.load_org(self.slug)
        fn(org)
        store.save_org(org)

    def test_rows_keep_search_attention_and_references_but_details_are_on_open(self):
        response = self.get()
        self.assertEqual(response.status_code, 200, response.text)
        body = response.json()
        self.assertNotIn("backlogged", body)
        self.assertNotIn("archived", body)
        self.assertEqual({r["slug"] for r in body["references"]}, {"visible", "hidden"})
        row = body["items"][0]
        self.assertEqual(row["objective"], "Searchable full description")
        self.assertEqual(row["view"], "list")
        self.assertNotIn("evidence", row)
        self.assertLess(len(response.content), 10000)
        self.assertEqual(body["attention"][0]["manual_attention"]["reason"], "Please decide")
        self.assertIn("manual", body["attention"][0]["attention_sources"])
        detail = self.client.get(f"/api/orgs/{self.slug}/work-items/visible", headers=OP).json()["item"]
        self.assertEqual(detail["evidence"][0]["note"], "heavy evidence " * 10000)
        opened = self.get(query="?backlogged=1").json()
        self.assertEqual(opened["backlogged"][0]["slug"], "hidden")

    def test_status_save_does_not_rebuild_or_retransmit(self):
        before = self.get()
        self.edit(lambda org: org.nodes["boss"].update(last_status={"status": "working", "summary": "new"}))
        with patch.object(work_ui, "_build", side_effect=AssertionError("unrelated save rebuilt docket")):
            after = self.get(before.headers["etag"])
        self.assertEqual(after.status_code, 304)
        self.assertEqual(after.content, b"")

    def test_delta_updates_and_removes_rows_and_keeps_unchanged_out(self):
        before = self.get(query="?backlogged=1")
        def edit(org):
            org.d["work_items"][0].update(title="Changed", rev=2)
            org.d["work_items"][1]["status"] = "open"
        self.edit(edit)
        after = self.get(before.headers["etag"], "?backlogged=1").json()
        self.assertEqual(after["base"], before.json()["revision"])
        self.assertEqual(after["delta"]["backlogged"]["order"], [])
        self.assertEqual({r["slug"] for r in after["delta"]["items"]["upsert"]}, {"visible", "hidden"})
        self.assertTrue(after["delta"]["attention"]["upsert"])

    def test_owner_incarnation_invalidates_without_item_rev_change(self):
        before = self.get()
        self.edit(lambda org: org.nodes["boss"].update(state="archived"))
        after = self.get(before.headers["etag"])
        self.assertEqual(after.status_code, 200)
        self.assertEqual(after.json()["delta"]["items"]["upsert"][0]["owner_state"], "retired")

    def test_question_change_invalidates_without_item_rev_change(self):
        before = self.get()
        self.edit(lambda org: org.d.setdefault("asks", []).append({
            "id": "q", "node": "boss", "status": "open", "kind": "question", "rev": 1,
            "questions": [{"question": "Decision?", "work_item": "visible"}]}))
        after = self.get(before.headers["etag"])
        self.assertEqual(after.status_code, 200)
        self.assertTrue(after.json()["delta"]["items"]["upsert"][0]["questions"])

    def test_unknown_base_reset_and_org_isolation(self):
        response = self.get('"expired"')
        self.assertNotIn("delta", response.json())
        other = self.client.get("/api/orgs/no-such-org/work-items-view", headers={**OP, "If-None-Match": response.headers["etag"]})
        self.assertEqual(other.status_code, 404)
        denied = self.client.get(self.url)
        self.assertIn(denied.status_code, (401, 403))

    def test_clock_archives_without_write(self):
        self.edit(lambda org: org.d["work_items"][0].update(status="done", docket_at="2000-01-01T00:00:00Z"))
        response = self.get().json()
        self.assertNotIn("visible", [r["slug"] for r in response["items"]])
        self.assertTrue(next(r for r in response["references"] if r["slug"] == "visible")["archived"])

    def test_delta_excludes_unchanged_rows(self):
        before = self.get()
        self.edit(lambda org: org.d["work_items"][0].update(rev=2))
        after = self.get(before.headers["etag"]).json()
        self.assertEqual([r["slug"] for r in after["delta"]["items"]["upsert"]], ["visible"])
        self.assertEqual(after["delta"]["attention"]["upsert"], [])


if __name__ == "__main__":
    unittest.main()
