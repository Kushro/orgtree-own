"""The four `*/usage/peek` routes run on the event loop, with no threadpool
hop and no response-model validation, and answer byte-for-byte what they
answered as sync routes.

N1000 engprof (2026-09-28): the peeks were 59% of all requests. As sync
`def`s with a `dict[str, Any]` return annotation, each paid two threadpool
hops, one for the handler and one for response validation (~145 + 150 ms
queued at N1000, p50 ~230 ms for a ~1 ms read). What this proves:
  * each route's endpoint is a coroutine function and has no response model;
  * serving each route makes ZERO `run_in_threadpool` calls. The NEGATIVE
    CONTROL is the old shape (a sync route with the same annotation), which
    makes at least 2 through the same counter, so the counter works;
  * the response bytes equal the old shape's bytes for every kind of peek
    payload (unavailable, stale, available with float/int/unicode/null
    limits).

Run:  python tools/run-python-verification.py tests/test_usage_peek_async.py
"""
import inspect
import os
from pathlib import Path
import sys
import tempfile
from typing import Any
import unittest
from unittest.mock import patch

_root = tempfile.TemporaryDirectory(prefix="orgtree-usage-peek-")
os.environ["ORGTREE_DATA"] = _root.name
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "engine/backend"))

import import_provenance  # noqa: F401,E402

from fastapi import FastAPI, routing  # noqa: E402
from fastapi.testclient import TestClient  # noqa: E402

from orgtree import api, antigravity_limits, codex_limits, limits, openrouter_limits  # noqa: E402

ROUTES = {
    "/api/usage/peek": limits,
    "/api/codex/usage/peek": codex_limits,
    "/api/antigravity/usage/peek": antigravity_limits,
    "/api/openrouter/usage/peek": openrouter_limits,
}
PAYLOADS = [
    {"available": False},
    {"available": False, "provider": "Codex", "error": "Codex usage readout is stale"},
    {"available": True, "provider": "Antigravity", "age": 12.3,
     "limits": [{"key": "five_hour", "label": "Session — 5 h", "utilization": 41.5,
                 "resets_at": "2026-09-28T17:00:00Z", "used": 3, "cap": None},
                {"key": "weekly", "label": "Weekly ✓", "utilization": 100,
                 "resets_at": None, "extra": {"nested": [1, 2.5, "x"]}}]},
    {"available": True, "limits": [], "age": 0.0},
]


def _old_shape_app(module) -> FastAPI:
    """The route as it was: a sync `def` annotated `-> dict[str, Any]`."""
    ref = FastAPI()

    @ref.get("/peek")
    def peek() -> dict[str, Any]:
        return module.peek()
    return ref


def _counting():
    n = [0]
    real = routing.run_in_threadpool

    async def counted(func, *args, **kwargs):
        n[0] += 1
        return await real(func, *args, **kwargs)
    return n, patch.object(routing, "run_in_threadpool", counted)


class UsagePeekAsync(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = TestClient(api.app)

    def _route(self, path):
        (route,) = [r for r in api.app.routes if getattr(r, "path", None) == path]
        return route

    def test_routes_are_coroutines_without_response_model(self) -> None:
        for path in ROUTES:
            with self.subTest(path=path):
                route = self._route(path)
                self.assertTrue(inspect.iscoroutinefunction(route.endpoint))
                self.assertIsNone(route.response_model)
                self.assertIsNone(route.response_field)

    def test_no_threadpool_hop_and_the_counter_works(self) -> None:
        for path, module in ROUTES.items():
            with self.subTest(path=path), patch.object(module, "peek", return_value=PAYLOADS[2]):
                n, p = _counting()
                with p:
                    self.assertEqual(self.client.get(path).status_code, 200)
                self.assertEqual(n[0], 0, "the peek went through the threadpool")
                n, p = _counting()
                with p:
                    self.assertEqual(TestClient(_old_shape_app(module)).get("/peek").status_code, 200)
                self.assertGreaterEqual(n[0], 2, "negative control: the old shape's hops were not counted")

    def test_bytes_identical_to_the_old_shape(self) -> None:
        checked = 0
        for path, module in ROUTES.items():
            for payload in PAYLOADS:
                with self.subTest(path=path, payload=payload), \
                        patch.object(module, "peek", return_value=payload):
                    new = self.client.get(path)
                    old = TestClient(_old_shape_app(module)).get("/peek")
                    self.assertEqual(new.status_code, old.status_code)
                    self.assertEqual(new.headers["content-type"], old.headers["content-type"])
                    self.assertEqual(new.content, old.content)
                    checked += 1
        self.assertEqual(checked, len(ROUTES) * len(PAYLOADS))

    def test_real_peek_without_a_patch_still_answers(self) -> None:
        for path in ROUTES:
            with self.subTest(path=path):
                r = self.client.get(path)
                self.assertEqual(r.status_code, 200)
                self.assertIn("available", r.json())


if __name__ == "__main__":
    unittest.main()
