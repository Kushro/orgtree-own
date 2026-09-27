"""Actual-PG listing snapshot, funding, privacy and compatibility contracts."""
import json
import os
import unittest
from unittest.mock import patch

import test_org_summary_behavior_pg as behavior
from orgtree import api, org_listing, org_summary, pgstore, store

tearDownModule = behavior.tearDownModule


@unittest.skipUnless(behavior.fixture.fixture.fixture.ADMIN, 'private PostgreSQL required: NOT RUN')
class SummaryReads(unittest.TestCase):
    setUpClass = behavior.SummaryBehavior.__dict__['setUpClass']
    setUp = behavior.SummaryBehavior.setUp
    query = behavior.SummaryBehavior.query
    setting = behavior.SummaryBehavior.setting
    seed = behavior.SummaryBehavior.seed
    row = behavior.SummaryBehavior.row

    def test_supported_admin_and_public_outputs_match_legacy_except_approved_rounding(self):
        self.seed()
        with patch.object(org_listing, '_native', return_value=False):
            old_admin, old_public = self.row(), self.row(public=True)
        with patch.object(store, 'list_orgs', side_effect=AssertionError('full listing')), \
                patch.object(store, 'list_orgs_with_docs', side_effect=AssertionError('full docs')), \
                patch.object(store, '_load_lazy', side_effect=AssertionError('whole nodes')):
            self.assertEqual(self.row(), old_admin)
            self.assertEqual(self.row(public=True), old_public)
            self.assertEqual([r['slug'] for r in org_summary.public_rows(self.slug)], [self.slug])

    def test_native_updates_retirement_and_rehire_refresh_counts_and_holds(self):
        self.seed()
        for state, live in [('archived', 0), ('live', 1)]:
            node = store.load_org(self.slug).node('worker')
            node.update(state=state, grant=9, cost_usd=3.123456)
            self.query('UPDATE nodes SET val=? WHERE id=?', (json.dumps(node), 'worker'))
            full = store.load_org(self.slug)
            actual = self.row()
            self.assertEqual(actual['nodes'], 3)
            self.assertEqual(actual['live'], live)
            self.assertEqual(actual['kiosk_cfg']['held'], full.audit()['top_level_holds'])
            self.assertEqual(actual['cost_usd_total'], full.cost_total())

    def test_metadata_funding_and_cost_share_one_committed_snapshot(self):
        import psycopg
        self.seed()
        old = self.row()
        execute = psycopg.Connection.execute
        switched = []
        with store._POOL.acquire(self.slug) as conn:
            schema = 'org_' + str(conn.org_id)
        def concurrent(conn, sql, params=None, **kw):
            cursor = execute(conn, sql, params, **kw)
            if 'FROM public.orgs o CROSS JOIN foreground_meta f' in str(sql) and not switched:
                switched.append(True)
                with pgstore.connect(os.environ['ORGTREE_PG_URL']) as other:
                    other.execute(f"UPDATE {schema}.nodes SET val=jsonb_set(val::jsonb,'{{state}}','\"archived\"')::text WHERE id='worker'")
                    other.execute(f"UPDATE {schema}.doc SET val=%s WHERE key='name'", (json.dumps('Changed'),))
                    other.execute(f"UPDATE {schema}.doc SET val='100' WHERE key='deleted_cost_usd'")
            return cursor
        with patch.object(psycopg.Connection, 'execute', concurrent):
            # One org read, so unrelated fixtures cannot trigger the writer.
            row, ctx = org_summary._read(self.slug, False)
        self.assertEqual(len(switched), 1)
        self.assertEqual((row['name'], row['live'], ctx.cost_total(), org_summary.top_level_holds(ctx)),
                         (old['name'], old['live'], old['cost_usd_total'], old['kiosk_cfg']['held']))
        current = self.row()
        self.assertEqual((current['name'], current['live']), ('Changed', 0))
        self.assertGreater(current['cost_usd_total'], old['cost_usd_total'])

    def test_projection_is_not_persistable_and_storage_cache_keeps_its_contract(self):
        self.seed()
        _, ctx = org_summary._read(self.slug, False)
        with self.assertRaises(TypeError): store.save_org(ctx)
        full = store.load_org(self.slug)
        for key in ('slug', 'workspace', 'sandbox', 'disk', 'kiosk'):
            self.assertEqual(ctx.d.get(key), full.d.get(key))
        calls = []
        class InlineThread:
            def __init__(self, target, **kw): self.target = target
            def start(self): self.target()
        with patch.object(api.supervisor, '_ws_usage_cache', {self.slug: (0, 123)}), \
                patch.object(api.supervisor, '_ws_walk_inflight', set()), \
                patch.object(api.supervisor, 'workspace_usage_bytes', side_effect=lambda org: calls.append(org.d['slug'])), \
                patch.object(api.supervisor.threading, 'Thread', InlineThread):
            self.assertEqual(api.supervisor.workspace_usage_cached(ctx), 123)
        self.assertEqual(calls, [self.slug])

    def test_legacy_settings_use_per_org_fallback_and_bad_number_refuses(self):
        self.seed()
        self.query("DELETE FROM doc WHERE key='_migrations'")
        load = store._load_lazy
        observed = []
        def watched(conn, slug, *args, **kw):
            observed.append(slug)
            return load(conn, slug, *args, **kw)
        with patch.object(store, '_load_lazy', watched):
            self.assertTrue(self.row()['kiosk'])
        self.assertIn(self.slug, observed)
        self.setting('deleted_cost_usd', 'not-a-number')
        with self.assertRaises(ValueError): self.row()


if __name__ == '__main__': unittest.main()
