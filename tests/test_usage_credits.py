"""Extra usage credits ride the usage board when the provider reports them.

Docket `v3-usage-panel-show-extra-usage-credits-per-acco`. openai/primary read
100% weekly while holding ~62,000 Codex credits, so the panel made a working
account look exhausted. The engine now carries the balance the provider
itself reports, and nothing when it reports none.

  §1 Codex: `account/rateLimits/read` snapshots carry
     `credits {hasCredits, unlimited, balance}` (measured 2026-10-01; the
     balance is a decimal string). `_normalize` copies it onto the board as
     `credits`, on the ambient read and a pinned-home read alike, and leaves
     the window rows exactly as they were.
  §2 Codex: no credits, a zero or unreadable balance -> no `credits` key at all.
  §3 Claude: `/api/oauth/usage` carries `spend.balance`, a money object like
     `spend.used`; the measured null (extra usage off) shows nothing, and the
     host read and a profile read both carry a real balance.
"""
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

_root = tempfile.TemporaryDirectory(prefix="v3-usage-credits-")
os.environ["ORGTREE_DATA"] = _root.name
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "engine" / "backend"))

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout

from orgtree import codex_limits, codexrun, limits, providers, subproxy  # noqa: E402

# the shape openai/primary returned on 2026-10-01 (account id dropped)
MEASURED_CODEX = {
    "rateLimits": {
        "limitId": "codex", "limitName": None,
        "primary": {"usedPercent": 100, "windowDurationMins": 10080,
                    "resetsAt": 1791047037},
        "secondary": None,
        "credits": {"hasCredits": True, "unlimited": False,
                    "balance": "62036.6481075000"},
        "planType": "pro", "rateLimitReachedType": "rate_limit_reached"},
}


def codex_raw(credits=None, where="codex"):
    snap = {"limitId": "codex", "planType": "pro",
            "primary": {"usedPercent": 100, "windowDurationMins": 10080,
                        "resetsAt": 1791047037}}
    extra = {"limitId": "gpt-reserve", "limitName": "GPT reserve",
             "primary": {"usedPercent": 3, "windowDurationMins": 10080,
                         "resetsAt": 1791047037}}
    if credits is not None:
        (snap if where == "codex" else extra)["credits"] = credits
    return {"rateLimits": snap,
            "rateLimitsByLimitId": {"codex": snap, "gpt-reserve": extra}}


class FakeClient:
    payload: dict = MEASURED_CODEX

    def __init__(self, argv_head, codex_home=None, **kw):
        pass

    def initialize(self):
        pass

    def request(self, method, params, timeout=None):
        assert method == "account/rateLimits/read"
        return json.loads(json.dumps(FakeClient.payload))

    def close(self):
        pass


class CodexCreditsTests(unittest.TestCase):
    def setUp(self):
        codex_limits.invalidate()
        codex_limits._home_cache.clear()
        self.addCleanup(codex_limits.invalidate)
        FakeClient.payload = MEASURED_CODEX

    # §1
    def test_the_measured_balance_rides_the_board(self):
        out = codex_limits._normalize(MEASURED_CODEX)
        self.assertEqual(out["credits"], {"balance": 62036.6481075,
                                          "unit": "credits", "unlimited": False})

    def test_window_rows_are_unchanged_by_credits(self):
        plain = codex_limits._normalize(codex_raw())
        rich = codex_limits._normalize(codex_raw(
            {"hasCredits": True, "unlimited": False, "balance": "12.5"}))
        self.assertNotIn("credits", plain)
        self.assertEqual(rich.pop("credits")["balance"], 12.5)
        self.assertEqual(plain, rich)

    def test_credits_on_another_bucket_are_still_found(self):
        out = codex_limits._normalize(codex_raw(
            {"hasCredits": True, "unlimited": False, "balance": "40"},
            where="gpt-reserve"))
        self.assertEqual(out["credits"]["balance"], 40.0)

    def test_unlimited_credits(self):
        out = codex_limits._normalize(codex_raw(
            {"hasCredits": True, "unlimited": True, "balance": None}))
        self.assertEqual(out["credits"], {"balance": None, "unit": "credits",
                                          "unlimited": True})

    def test_the_ambient_read_serves_credits(self):
        status = {"installed": True, "connected": True, "kind": "chatgpt"}
        with mock.patch.object(codex_limits, "account_namespace", return_value="codex-chatgpt:a"), \
             mock.patch.object(providers, "codex_status", return_value=status), \
             mock.patch.object(providers, "codex_path", return_value=("codex.exe", "test")), \
             mock.patch.object(providers, "codex_argv", side_effect=lambda exe: [exe]), \
             mock.patch.object(codexrun, "AppServerClient", side_effect=FakeClient):
            out = codex_limits.fetch(force=True)
            cached = codex_limits.fetch()
        self.assertEqual(out["credits"]["balance"], 62036.6481075)
        self.assertEqual(cached["credits"]["balance"], 62036.6481075)
        self.assertEqual(out["limits"][0]["percent"], 100.0)

    def test_a_pinned_home_read_serves_credits(self):
        with mock.patch.object(providers, "codex_path", return_value=("codex.exe", "test")), \
             mock.patch.object(providers, "codex_argv", side_effect=lambda exe: [exe]), \
             mock.patch.object(codexrun, "AppServerClient", side_effect=FakeClient):
            out = codex_limits.fetch_for_home(_root.name, "acct:openai-1", force=True)
        self.assertEqual(out["credits"]["balance"], 62036.6481075)

    # §2
    def test_nothing_reported_means_no_credits_key(self):
        for credits in (
                None,
                {"hasCredits": False, "unlimited": False, "balance": "500"},
                {"hasCredits": True, "unlimited": False, "balance": "0"},
                {"hasCredits": True, "unlimited": False, "balance": "0.0000000"},
                {"hasCredits": True, "unlimited": False, "balance": "-3"},
                {"hasCredits": True, "unlimited": False, "balance": None},
                {"hasCredits": True, "unlimited": False, "balance": "lots"},
                {"hasCredits": True, "unlimited": False, "balance": "nan"},
                {"hasCredits": True, "unlimited": False, "balance": "inf"},
                # review-sol F2: float(True) == 1.0, never one credit
                {"hasCredits": True, "unlimited": False, "balance": True},
                {"hasCredits": True, "unlimited": False, "balance": False},
                {"hasCredits": "yes", "unlimited": False, "balance": "500"},
                "62036"):
            with self.subTest(credits=credits):
                self.assertNotIn("credits", codex_limits._normalize(codex_raw(credits)))


# ── §3 Claude ───────────────────────────────────────────────────────────────

MEASURED_CLAUDE_SPEND = {  # claude/primary on 2026-10-01: extra usage off
    "used": {"amount_minor": 0, "currency": "USD", "exponent": 2},
    "limit": None, "percent": 0, "severity": "normal", "enabled": False,
    "cap": None, "balance": None, "auto_reload": None}


def claude_raw(balance):
    spend = dict(MEASURED_CLAUDE_SPEND, balance=balance)
    return {"five_hour": {"utilization": 6, "resets_at": "2026-10-01T11:20:00Z"},
            "seven_day": {"utilization": 30, "resets_at": "2026-10-06T18:00:00Z"},
            "spend": spend}


class ClaudeCreditsTests(unittest.TestCase):
    def setUp(self):
        limits.invalidate()
        self.addCleanup(limits.invalidate)

    def test_a_money_balance_is_read(self):
        self.assertEqual(
            limits._credits(claude_raw({"amount_minor": 1250, "currency": "usd",
                                        "exponent": 2})),
            {"balance": 12.5, "unit": "USD", "unlimited": False})

    def test_nothing_reported_means_no_credits(self):
        for balance in (None, 0, 1250, "12.50",
                        {"amount_minor": 0, "currency": "USD", "exponent": 2},
                        {"amount_minor": -5, "currency": "USD", "exponent": 2},
                        {"amount_minor": 100, "currency": "", "exponent": 2},
                        {"amount_minor": 100, "exponent": 2},
                        {"amount_minor": True, "currency": "USD", "exponent": 2},
                        {"amount_minor": 100, "currency": "USD", "exponent": 2.0},
                        {"amount_minor": 100, "currency": "USD", "exponent": 99}):
            with self.subTest(balance=balance):
                self.assertIsNone(limits._credits(claude_raw(balance)))
        self.assertIsNone(limits._credits({}))
        self.assertIsNone(limits._credits({"spend": None}))

    def _serve(self, raw):
        def urlopen(req, timeout=None):
            return io.BytesIO(json.dumps(raw).encode())
        return mock.patch.object(limits.urllib.request, "urlopen", side_effect=urlopen)

    def _host_fetch(self, raw):
        with self._serve(raw), \
             mock.patch.object(limits, "_identity", return_value={"uuid": "u1", "email": "a@b"}), \
             mock.patch.object(limits, "_plan", return_value="Max"), \
             mock.patch.object(subproxy, "available", return_value=True), \
             mock.patch.object(subproxy, "get_access_token", return_value="tok"):
            return limits.fetch(force=True)

    def test_the_host_read_carries_a_balance(self):
        out = self._host_fetch(claude_raw({"amount_minor": 50000, "currency": "USD",
                                           "exponent": 2}))
        self.assertEqual(out["credits"]["balance"], 500.0)
        self.assertEqual([lim["percent"] for lim in out["limits"]], [6, 30])

    def test_the_measured_host_answer_shows_no_credits(self):
        out = self._host_fetch(claude_raw(None))
        self.assertTrue(out["available"])
        self.assertNotIn("credits", out)

    def test_a_profile_read_carries_a_balance(self):
        with self._serve(claude_raw({"amount_minor": 700, "currency": "EUR",
                                     "exponent": 2})):
            out = limits.fetch_for_token("tok", "acct:claude-9", force=True)
        self.assertEqual(out["credits"], {"balance": 7.0, "unit": "EUR",
                                          "unlimited": False})


if __name__ == "__main__":
    unittest.main()
