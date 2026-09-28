"""The N1000 controller's rows preflight verdict (tools/scale/rows_preflight.py)."""
from pathlib import Path
import sys
import unittest

import import_provenance  # noqa: F401
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools/scale"))
from rows_preflight import verdict


def arm(read, message, tokens=1, harness=5):
    return {"tokens": {"rows": tokens}, "settlement": {"rows": harness}, "work_items": {"rows": read},
            "chat": {"rows": read}, "message": {"rows": message}}


class Verdict(unittest.TestCase):
    def test_flat_rows_pass(self):
        passed, ratios = verdict({10: arm(40, 90), 100: arm(44, 120, tokens=100)})
        self.assertTrue(passed)
        self.assertAlmostEqual(ratios["message"], 120 / 90)

    def test_whole_org_load_per_request_fails(self):
        # Shape of efb74da at N1000: rows track the node count on every call.
        passed, ratios = verdict({10: arm(45, 180), 100: arm(415, 1660)})
        self.assertFalse(passed)
        self.assertGreater(ratios["chat"], 2)

    def test_one_growing_call_is_enough_to_fail(self):
        passed, _ = verdict({10: arm(40, 90), 100: arm(40, 900)})
        self.assertFalse(passed)

    def test_harness_and_per_agent_calls_are_not_judged(self):
        passed, ratios = verdict({10: arm(40, 90, tokens=10, harness=97),
                                  100: arm(40, 90, tokens=1000, harness=359)})
        self.assertTrue(passed)
        self.assertEqual(set(ratios), {"work_items", "chat", "message"})

    def test_no_measured_work_refuses_a_verdict(self):
        with self.assertRaises(ValueError):
            verdict({10: arm(0, 90), 100: arm(0, 90)})
        with self.assertRaises(ValueError):
            verdict({10: {"message": {"rows": 4}}, 100: {"message": {"rows": 4}}})


if __name__ == "__main__":
    unittest.main()
