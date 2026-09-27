"""Actual-PG coherent policy snapshots, authority and history bounds."""
import copy
import json
import os
from pathlib import Path
import unittest
from unittest.mock import patch

import test_mail_archive_bounds_pg as fixture
from engine.launch import load_app
load_app()
from orgtree import ledger, pgstore, policy_reads, store, supervisor as sup

tearDownModule = fixture.tearDownModule


@unittest.skipUnless(fixture.ADMIN, 'ORGTREE_TEST_PG_ADMIN_URL not set: NOT RUN')
class PolicyReads(unittest.TestCase):
    setUpClass = classmethod(fixture.MailArchiveBounds.setUpClass.__func__)
    setUp = fixture.MailArchiveBounds.setUp
    query = fixture.MailArchiveBounds.query

    def configure(self, state='live', frozen=False):
        org = store.load_org(self.slug)
        n = org.node('worker')
        n['state'] = state
        if frozen: n['frozen'] = {'limit': True}
        org.d['watchdogs'] = [dict(id='dog', owner='worker', kind='process', state='armed',
                                  name='dog', target='pid:123', interval_s=15)]
        store.save_org(org)
        return org

    def test_selected_owner_decisions_match_full_org_and_refresh_after_retirement(self):
        self.configure(frozen=True)
        full = store.load_org(self.slug)
        got = policy_reads.watchdog_org(self.slug)
        self.assertEqual(sup._wd_owner_lost(got, got.d['watchdogs'][0]),
                         sup._wd_owner_lost(full, full.d['watchdogs'][0]))
        self.assertIsNone(sup._wd_owner_lost(got, got.d['watchdogs'][0]))
        self.configure(state='archived')
        got = policy_reads.watchdog_org(self.slug)
        self.assertEqual(sup._wd_owner_lost(got, got.d['watchdogs'][0]), ledger.Org.WATCHDOG_ARCHIVE_PAUSE)

    def test_storage_fields_and_unknown_node_blob_fallback(self):
        org = self.configure()
        org.d.update(kiosk={'enabled': False, 'storage_limit_mb': 3}, storage_blocked={'at': 'x'})
        store.save_org(org)
        got = policy_reads.storage_org(self.slug)
        self.assertEqual(got.d['kiosk'], org.d['kiosk'])
        self.assertEqual(got.d['storage_blocked'], org.d['storage_blocked'])
        self.assertEqual(got.nodes, {})
        with store._POOL.acquire(self.slug) as conn:
            conn.execute("INSERT INTO doc(key,val) VALUES('nodes','{}')")
        with patch.object(store, 'cached_org', return_value=org) as fallback:
            self.assertIs(policy_reads.watchdog_org(self.slug), org)
        fallback.assert_called_once_with(self.slug)

    def test_owner_and_watchdog_use_one_statement_snapshot(self):
        self.configure()
        real = pgstore.PgConn.execute
        changed = []
        def interleave(conn, sql, params=()):
            result = real(conn, sql, params)
            if sql.startswith('WITH settings') and not changed:
                changed.append(True)
                # Independent writer after SELECT executes, before fetch/decode.
                with pgstore.connect(os.environ['ORGTREE_PG_URL']) as writer:
                    writer.execute(f"UPDATE org_{conn.org_id}.nodes SET val="
                                   "jsonb_set(val::jsonb,'{state}','\"archived\"')::text WHERE id='worker'")
                    writer.execute(f"UPDATE org_{conn.org_id}.doc SET val='[]' WHERE key='watchdogs'")
            return result
        with patch.object(pgstore.PgConn, 'execute', interleave):
            got = policy_reads.watchdog_org(self.slug)
        self.assertTrue(changed)
        self.assertEqual(got.nodes['worker']['state'], 'live')
        self.assertEqual(len(got.d['watchdogs']), 1)
        self.assertEqual(policy_reads.watchdog_org(self.slug).d['watchdogs'], [])

    def test_bounded_owners_and_statement_plan_at_real_fleet_shape(self):
        org = self.configure()
        seed = org.node('worker')
        with store._POOL.acquire(self.slug) as conn:
            for i in range(1183):
                raw = dict(seed, state='live' if i < 29 else 'archived', charter='history ' * 100)
                conn.execute('INSERT INTO nodes(id,ord,val) VALUES(?,?,?)',
                             (f'old-{i:04}', i + 100, json.dumps(raw)))
            conn.execute('ANALYZE nodes')
        real = pgstore.PgConn.execute
        plans = []
        def observed(conn, sql, params=()):
            if sql.startswith('WITH settings'):
                plans.append(real(conn, 'EXPLAIN (ANALYZE, FORMAT JSON) ' + sql, params).fetchone()[0])
            return real(conn, sql, params)
        with patch.object(pgstore.PgConn, 'execute', observed), \
                patch.object(store, 'cached_org', side_effect=AssertionError('full Org read')):
            got = policy_reads.watchdog_org(self.slug)
            storage = policy_reads.storage_org(self.slug)
        self.assertEqual(set(got.nodes), {'worker'})
        self.assertEqual(storage.nodes, {})
        self.assertEqual(len(plans), 2)
        def walk(node):
            if node.get('Relation Name') == 'nodes':
                self.assertLessEqual(node['Actual Rows'], 1)
                self.assertIn('Index', node['Node Type'])
            for child in node.get('Plans', []): walk(child)
        for plan in plans: walk(plan[0]['Plan'])


if __name__ == '__main__': unittest.main()
