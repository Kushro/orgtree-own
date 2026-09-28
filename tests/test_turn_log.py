"""ORGTREE_TURN_LOG: a node's full turn history in the `turn_log` dict log.

The node row keeps its newest TREE_TURNS turns plus `turn_seq` and the running
sums the killed-turn estimate needs. What these prove:
  * EXACT: the running sums equal the old `sum()` over the whole ring — same
    value, same type — on random histories, on a long legacy ring converted
    and then N more turns through both paths, and so the estimate is equal;
  * ROLLBACK: converted -> an older engine appends to the ring -> roll-forward
    logs every turn once, in order (idempotent, keyed by the turn number);
  * the switch off is the old ring append;
  * PostgreSQL: the whole-load heal converts and commits the log rows; a
    turn is appended WITHOUT reading the node's history; a transaction that
    did not name the log still writes an unconverted node; the three
    supervisor writers name the log (without it their writes are refused).

Run:  python tools/run-python-verification.py tests/test_turn_log.py
"""
import contextlib
import copy
import json
import random
import unittest
import uuid
from unittest.mock import patch

import test_pgstore as f
from orgtree import history, ledger, orgtx, pgstore, store


def tearDownModule():
    f.tearDownModule()


def old_sums(ring):
    """supervisor._charge_killed_turn before ORGTREE_TURN_LOG, verbatim."""
    pairs = [(t.get("cost") or 0.0, t.get("toks") or 0)
             for t in ring
             if t.get("cost") and t.get("toks") and not t.get("killed")]
    return sum(c for c, _ in pairs), sum(tk for _, tk in pairs)


def estimate(sums, out_toks=1234):
    num, den = sums
    return round(out_toks * num / den, 6) if (out_toks and den) else 0.0


def turn(rnd, i):
    r = rnd.random()
    e = {"at": f"2026-09-28T00:00:{i % 60:02d}Z", "i": i,
         "toks": rnd.choice([0, 1, 57, 1200, 88000])}
    if r < 0.1:
        e["cost"] = 0
    elif r < 0.2:
        e["cost"] = rnd.randint(1, 3)                      # an int cost
    elif r < 0.25:
        e["cost"] = rnd.choice([1e16, 1e-12, 0.1])         # magnitudes that drift
    else:
        e["cost"] = round(rnd.uniform(0, 3) * 10 ** rnd.randint(-5, 1), rnd.randint(2, 9))
    if rnd.random() < 0.1:
        e["killed"] = True
    return e


def strip(rows):
    return [{k: v for k, v in r.items() if k != "n"} for r in rows]


class TurnSums(unittest.TestCase):
    def setUp(self):
        flag = patch.object(ledger, "TURN_LOG", True)
        flag.start()
        self.addCleanup(flag.stop)

    def converted(self, legacy, more):
        """A node with `legacy` un-logged ring entries, converted, then
        `more` turns through record_turn. Returns (doc, node)."""
        d = {}
        n = {"turns": copy.deepcopy(legacy)}
        ledger.convert_turns(d, "a", n)
        for e in copy.deepcopy(more):
            ledger.record_turn(d, "a", n, e)
        return d, n

    def test_running_sums_equal_the_whole_ring_sum_exactly(self):
        rnd = random.Random(20260928)
        for trial in range(400):
            hist = [turn(rnd, i) for i in range(rnd.randint(0, 60))]
            cut = rnd.randint(0, len(hist))
            d, n = self.converted(hist[:cut], hist[cut:])
            got, want = ledger.turn_estimate_sums(n), old_sums(hist)
            self.assertEqual(got, want, trial)
            self.assertEqual([type(v) for v in got], [type(v) for v in want], trial)
            self.assertEqual(strip(d.get("turn_log", {}).get("a", [])), hist, trial)
            self.assertEqual(strip(n["turns"]), hist[-ledger.TREE_TURNS:], trial)
            self.assertEqual(n["turn_seq"], len(hist))

    def test_a_long_ring_then_n_more_turns_through_both_paths(self):
        rnd = random.Random(7)
        legacy = [turn(rnd, i) for i in range(600)]
        more = [turn(rnd, 600 + i) for i in range(45)]
        # the old path: every turn appended to the ring, the ring summed
        with patch.object(ledger, "TURN_LOG", False):
            old = {"turns": copy.deepcopy(legacy)}
            for e in copy.deepcopy(more):
                ledger.record_turn({}, "a", old, e)
        self.assertEqual(len(old["turns"]), 645)
        self.assertNotIn("turn_seq", old)
        # converted after the long ring, before the N more
        d, n = self.converted(legacy, [])
        self.assertEqual(ledger.turn_estimate_sums(n), old_sums(legacy))
        for e in copy.deepcopy(more):
            ledger.record_turn(d, "a", n, e)
        self.assertEqual(ledger.turn_estimate_sums(n), old_sums(old["turns"]))
        self.assertEqual(estimate(ledger.turn_estimate_sums(n)), estimate(old_sums(old["turns"])))
        self.assertEqual(ledger.turn_estimate_sums(old), old_sums(old["turns"]))
        self.assertEqual(len(n["turns"]), ledger.TREE_TURNS)
        self.assertEqual([r["n"] for r in d["turn_log"]["a"]], list(range(1, 646)))

    def test_rollback_older_engine_appends_then_roll_forward(self):
        rnd = random.Random(11)
        legacy = [turn(rnd, i) for i in range(20)]
        d, n = self.converted(legacy, [turn(rnd, 20 + i) for i in range(3)])
        full = d["turn_log"]["a"] + []
        self.assertEqual(len(full), 23)
        # an older engine (no turn log) appends to the ring: no `n`, no trim
        older = [turn(rnd, 23 + i) for i in range(12)]
        n["turns"].extend(copy.deepcopy(older))
        hist = strip(full) + older
        self.assertEqual(ledger.turn_estimate_sums(n), old_sums(hist))
        self.assertEqual(strip(history._turn_log_rows_from(d, n, "a")), hist)
        # roll forward: logged once each, in order, numbered on
        self.assertTrue(ledger.convert_turns(d, "a", n))
        self.assertEqual(strip(d["turn_log"]["a"]), hist)
        self.assertEqual([r["n"] for r in d["turn_log"]["a"]], list(range(1, 36)))
        self.assertEqual(ledger.turn_estimate_sums(n), old_sums(hist))
        # idempotent: converting again changes and logs nothing
        snap = copy.deepcopy((d, n))
        self.assertFalse(ledger.convert_turns(d, "a", n))
        self.assertEqual((d, n), snap)
        # and new turns carry on from there
        e = turn(rnd, 99)
        ledger.record_turn(d, "a", n, e)
        self.assertEqual(d["turn_log"]["a"][-1]["n"], 36)
        self.assertEqual(ledger.turn_estimate_sums(n), old_sums(hist + [strip([e])[0]]))

    def test_a_new_turn_logs_the_older_engines_appends_first(self):
        rnd = random.Random(3)
        d, n = self.converted([turn(rnd, i) for i in range(10)], [])
        older = [turn(rnd, 10 + i) for i in range(2)]
        n["turns"].extend(copy.deepcopy(older))
        e = turn(rnd, 12)
        ledger.record_turn(d, "a", n, e)
        self.assertEqual([r["i"] for r in d["turn_log"]["a"]], list(range(13)))
        self.assertEqual([r["n"] for r in d["turn_log"]["a"]], list(range(1, 14)))

    def test_the_switch_off_is_the_old_ring_append(self):
        with patch.object(ledger, "TURN_LOG", False):
            d, n = {}, {"turns": [{"cost": 0.1, "toks": 5}] * 9}
            ledger.record_turn(d, "a", n, {"cost": 0.2, "toks": 7})
        self.assertEqual(d, {})
        self.assertEqual(len(n["turns"]), 10)
        self.assertNotIn("n", n["turns"][-1])
        self.assertNotIn("turn_seq", n)

    def test_a_non_number_raises_where_the_old_sum_raised(self):
        ring = [{"cost": 0.1, "toks": 5}, {"cost": "x", "toks": 5}]
        with self.assertRaises(TypeError):
            old_sums(ring)
        with self.assertRaises(TypeError):
            ledger.turn_estimate_sums({"turns": copy.deepcopy(ring)})
        d, n = self.converted(ring, [])
        with self.assertRaises(TypeError):
            ledger.turn_estimate_sums(n)


class SwitchDefault(unittest.TestCase):
    def test_the_turn_log_default_is_off(self):
        import os
        if 'ORGTREE_TURN_LOG' in os.environ:
            self.skipTest('ORGTREE_TURN_LOG set in this environment: default NOT tested')
        self.assertFalse(ledger.TURN_LOG)


class Rename(unittest.TestCase):
    def test_a_rename_rekeys_the_turn_log(self):
        from test_authorized_review_workflow import fixture
        org, _ = fixture()
        org.d["turn_log"] = {"peer-b": [{"n": 1, "cost": 0.1}]}
        org.rename(ledger.USER, "peer-b", "peer-bb")
        self.assertEqual(org.d["turn_log"], {"peer-bb": [{"n": 1, "cost": 0.1}]})


def node(nid, **extra):
    return {'id': nid, 'name': nid, 'parent': None, 'children': [], 'payload': {'v': 1}, **extra}


@unittest.skipUnless(f.ADMIN, 'disposable PostgreSQL required: NOT RUN')
class PgTurnLog(unittest.TestCase):
    LEGACY = 30

    @classmethod
    def setUpClass(cls):
        store.claim_data_root()

    def setUp(self):
        flags = patch.multiple(store, LAZY_ROWS=True, ORGTX_RESCOPE=True, _heal_epoch_value=[])
        flags.start()
        self.addCleanup(flags.stop)
        flag = patch.object(ledger, 'TURN_LOG', True)
        flag.start()
        self.addCleanup(flag.stop)
        old = orgtx.use_backend(orgtx.PgBackend())
        self.addCleanup(orgtx.use_backend, old)
        rnd = random.Random(5)
        self.legacy = [turn(rnd, i) for i in range(self.LEGACY)]
        self.rnd = rnd
        with patch.object(ledger, 'TURN_LOG', False):     # stored by an engine before the log
            org = store.create_org('turnlog-' + uuid.uuid4().hex[:10])
            self.slug = org.d['slug']
            org.hire(ledger.USER, None, 'opus', 0, 'worker')
            org.node('worker')['turns'] = copy.deepcopy(self.legacy[:12])
            for i in range(6):
                org.d['nodes'][f'n{i}'] = node(f'n{i}')
            org.d['nodes']['n3']['turns'] = copy.deepcopy(self.legacy)
            store.save_org(org)
        with pgstore.connect() as raw:
            self.oid = raw.execute('SELECT org_id FROM public.orgs WHERE slug=%s',
                                   (self.slug,)).fetchone()[0]

    @contextlib.contextmanager
    def raw(self):
        with pgstore.connect() as raw:
            raw.execute('BEGIN')
            raw.execute(f'SET LOCAL search_path TO org_{self.oid},public')
            try:
                yield raw
            except BaseException:
                raw.execute('ROLLBACK')
                raise
            else:
                raw.execute('COMMIT')

    def node_row(self, nid='n3'):
        with self.raw() as raw:
            return json.loads(raw.execute('SELECT val FROM nodes WHERE id=%s', (nid,)).fetchone()[0])

    def log_rows(self, owner='n3'):
        with self.raw() as raw:
            return [json.loads(v) for (v,) in raw.execute(
                "SELECT val FROM log_d WHERE sect='turn_log' AND owner=%s ORDER BY seq",
                (owner,)).fetchall()]

    def stamp(self):
        with orgtx.org_tx(self.slug, nodes=['n0']):
            pass
        with self.raw() as raw:
            epoch = raw.execute("SELECT val FROM meta WHERE key='heal_epoch'").fetchone()
        self.assertEqual(epoch[0], store.heal_epoch())

    def test_the_whole_load_heal_converts_and_commits_the_log(self):
        self.stamp()
        n = self.node_row()
        self.assertEqual(n['turn_seq'], self.LEGACY)
        self.assertEqual(len(n['turns']), ledger.TREE_TURNS)
        self.assertEqual(strip(self.log_rows()), self.legacy)
        self.assertEqual([r['n'] for r in self.log_rows()], list(range(1, self.LEGACY + 1)))
        self.assertEqual(ledger.turn_estimate_sums(n), old_sums(self.legacy))
        self.stamp()                                 # a second load heals nothing more
        self.assertEqual(len(self.log_rows()), self.LEGACY)

    def test_a_turn_is_appended_without_reading_the_history(self):
        self.stamp()
        e = turn(self.rnd, 100)
        with orgtx.org_tx(self.slug, nodes=['n3'], logs=['turn_log']) as tx:
            ledger.record_turn(tx.org.d, 'n3', tx.org.node('n3'), dict(e))
            sec = dict.get(tx.org.d, 'turn_log')
            self.assertIsInstance(sec, store.SectionMap)
            self.assertFalse(dict.__contains__(sec, 'n3'), 'the history was read')
        with orgtx.org_tx(self.slug, nodes=['n3'], logs=['turn_log']) as tx:
            ledger.record_turn(tx.org.d, 'n3', tx.org.node('n3'), dict(e, i=101))
        rows = self.log_rows()
        self.assertEqual([r['n'] for r in rows], list(range(1, self.LEGACY + 3)))
        self.assertEqual(strip(rows), self.legacy + [e, dict(e, i=101)])
        self.assertEqual(ledger.turn_estimate_sums(self.node_row()), old_sums(strip(rows)))
        view = store.load_runtime_org(self.slug)
        self.assertEqual(strip(history._turn_log_rows(view, 'n3')), strip(rows))

    def test_an_older_engine_append_after_the_stamp_is_logged_once(self):
        self.stamp()
        # an older engine that never stamps an epoch appends two turns
        n = self.node_row()
        older = [turn(self.rnd, 200), turn(self.rnd, 201)]
        n['turns'] += older
        with self.raw() as raw:
            raw.execute('UPDATE nodes SET val=%s WHERE id=%s', (json.dumps(n), 'n3'))
        # a transaction that did not name the log still writes the node
        with orgtx.org_tx(self.slug, nodes=['n3']) as tx:
            tx.org.node('n3')['payload']['v'] = 2
        self.assertEqual(len(self.log_rows()), self.LEGACY)
        n = self.node_row()
        self.assertEqual(ledger.turn_estimate_sums(n), old_sums(self.legacy + older))
        e = turn(self.rnd, 202)
        with orgtx.org_tx(self.slug, nodes=['n3'], logs=['turn_log']) as tx:
            ledger.record_turn(tx.org.d, 'n3', tx.org.node('n3'), dict(e))
        rows = self.log_rows()
        self.assertEqual(strip(rows), self.legacy + older + [e])
        self.assertEqual([r['n'] for r in rows], list(range(1, self.LEGACY + 4)))

    def test_rollback_then_roll_forward_through_the_epoch(self):
        self.stamp()
        # an older engine with its own epoch loads, stamps, appends three
        n = self.node_row()
        older = [turn(self.rnd, 300 + i) for i in range(3)]
        n['turns'] += older
        with self.raw() as raw:
            raw.execute('UPDATE nodes SET val=%s WHERE id=%s', (json.dumps(n), 'n3'))
            raw.execute("UPDATE meta SET val='older-engine' WHERE key='heal_epoch'")
        self.stamp()                                 # roll-forward: whole load, heal
        rows = self.log_rows()
        self.assertEqual(strip(rows), self.legacy + older)
        self.assertEqual([r['n'] for r in rows], list(range(1, self.LEGACY + 4)))
        self.assertEqual(len(self.node_row()['turns']), ledger.TREE_TURNS)
        self.assertEqual(ledger.turn_estimate_sums(self.node_row()), old_sums(self.legacy + older))

    def test_the_supervisor_writers_log_their_turns(self):
        from orgtree import supervisor as sup
        self.stamp()
        with patch.object(sup, '_stamp_ran_as'):
            sup._charge_reported_spend(self.slug, 'n3', 0.25)
            sup._charge_killed_turn(self.slug, 'n3', 1000)
        rows = self.log_rows()
        self.assertEqual(len(rows), self.LEGACY + 2, 'a writer did not log its turn')
        killed = rows[-1]
        self.assertTrue(killed.get('killed'))
        self.assertEqual(killed.get('cost'), estimate(old_sums(strip(rows[:-1])), 1000))
        self.assertEqual(self.node_row()['turns'][-1]['n'], self.LEGACY + 2)

    def test_the_turn_end_writer_logs_its_turn(self):
        from orgtree import supervisor as sup
        self.stamp()
        self.assertEqual(len(self.log_rows('worker')), 12)
        with patch.object(sup, '_count_cli_compactions', return_value=(0, 0, [])), \
                patch.object(sup, 'session_occupancy', return_value=(None, False)), \
                patch.object(sup, 'notify'):
            org = store.load_org(self.slug)
            sup._after_turn(self.slug, 'worker', org, {'total_cost_usd': 0.05},
                            sup.state(self.slug, 'worker'))
        rows = self.log_rows('worker')
        self.assertEqual(len(rows), 13, 'the turn-end writer did not log its turn')
        self.assertEqual(rows[-1]['n'], 13)
        self.assertEqual(self.node_row('worker')['turn_seq'], 13)

    def test_switching_on_reheals_an_org_stamped_with_it_off(self):
        with patch.object(ledger, 'TURN_LOG', False), \
                patch.object(store, '_heal_epoch_value', []):
            self.stamp()
        self.assertNotIn('turn_seq', self.node_row())
        self.stamp()                        # on: a different epoch, a whole load
        self.assertEqual(self.node_row()['turn_seq'], self.LEGACY)
        self.assertEqual(len(self.log_rows()), self.LEGACY)

    def test_the_history_page_serves_every_turn(self):
        self.stamp()
        n = self.node_row()
        older = [turn(self.rnd, 400)]
        n['turns'] += older
        with self.raw() as raw:
            raw.execute('UPDATE nodes SET val=%s WHERE id=%s', (json.dumps(n), 'n3'))
        page = history.history_page(self.slug, 'turns', node='n3', limit=100)
        self.assertEqual(page['total'], self.LEGACY + 1)
        self.assertEqual(strip(list(reversed(page['items']))), self.legacy + older)

    def test_the_switch_off_writes_no_log(self):
        with patch.object(ledger, 'TURN_LOG', False), \
                patch.object(store, '_heal_epoch_value', []):
            self.stamp()
            with orgtx.org_tx(self.slug, nodes=['n3']) as tx:
                ledger.record_turn(tx.org.d, 'n3', tx.org.node('n3'), turn(self.rnd, 1))
        self.assertEqual(self.log_rows(), [])
        self.assertEqual(len(self.node_row()['turns']), self.LEGACY + 1)


if __name__ == '__main__':
    unittest.main()
