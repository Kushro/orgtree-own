"""Actual PostgreSQL lazy transaction reads; no skipped run is a success."""
import copy
import json
import os
import unittest
from unittest.mock import patch
import test_pgstore as f
from orgtree import orgtx, pgstore, store, workrows
from test_work_item_rows import items


def tearDownModule(): f.tearDownModule()


@unittest.skipUnless(f.ADMIN, 'disposable PG not configured: NOT RUN')
class LazyRows(unittest.TestCase):
    @classmethod
    def setUpClass(cls): store.claim_data_root()

    def setUp(self):
        self.slug=f._fresh_org('lazy-'+self._testMethodName)
        org=store.load_org(self.slug)
        org.d['work_items']=[dict(v, evidence=[{'body':'x'*131072}]) for v in items()]
        store.save_org(org)
        # Cold validation is explicit and separated from the warm read claim.
        with orgtx.org_tx(self.slug, nodes=['a']) as tx: pass

    def row(self, slug='one'):
        return store.read_work_items_rows(self.slug,[slug])['items'][slug]

    def watch(self):
        seen=[]; original=pgstore.PgConn.execute
        def execute(c, sql, params=()):
            result=original(c,sql,params)
            if 'SELECT' in sql and 'val' in sql and 'doc' in sql:
                seen.append((sql,params))
            return result
        return seen,patch.object(pgstore.PgConn,'execute',execute)

    def test_node_transaction_never_reads_work_bodies_after_validation(self):
        seen,watch=self.watch()
        with watch:
            with orgtx.org_tx(self.slug,nodes=['a']) as tx:
                self.assertIn('work_items', tx.d._deferred_doc)
                tx.d['nodes']['a']['last_status']={'summary':'node only'}
        self.assertFalse(any('ANY' in sql or 'xmin' in sql for sql,_ in seen),seen)
        self.assertEqual(self.row()['evidence'][0]['body'],'x'*131072)
        self.assertEqual(store.read_node(self.slug,'a')['last_status']['summary'],'node only')

    def test_one_item_access_fetches_only_that_item_nested_edit_survives(self):
        seen,watch=self.watch()
        with watch:
            with orgtx.org_tx(self.slug,sections=['work_items','asks']) as tx:
                rows=tx.d['work_items']
                self.assertFalse(rows[0]._loaded); self.assertFalse(rows[1]._loaded)
                rows[0]['nested']['values'].append(7)
                self.assertFalse(rows[1]._loaded)
        fetches=[p for sql,p in seen if 'SELECT val, xmin' in sql]
        self.assertEqual(fetches,[(workrows.PREFIX+'one',)])
        self.assertEqual(self.row()['nested']['values'],[1,7])
        self.assertEqual(self.row('two')['nested']['values'],[2])

    def test_external_alias_and_retained_nested_reference(self):
        external={'value':1}
        with orgtx.org_tx(self.slug,sections=['work_items','asks']) as tx:
            row=tx.d['work_items'][0]
            nested=row['nested']; nested['external']=external
            observed=nested['external']; external['value']=2
            nested['values'].append(8)
        self.assertEqual(self.row()['nested'],{'values':[1,8],'external':{'value':2}})

    def test_attention_update_only_loads_changed_item(self):
        seen,watch=self.watch()
        with watch:
            with orgtx.org_tx(self.slug,sections=['work_items','asks']) as tx:
                tx.d['asks']=[{'id':'q','status':'open','questions':[{'work_item':'one'}]}]
        self.assertEqual([p for sql,p in seen if 'SELECT val, xmin' in sql],[(workrows.PREFIX+'one',)])
        self.assertTrue(self.row()['notification_attention_active'])
        self.assertFalse(self.row('two')['notification_attention_active'])

    def test_stale_lazy_access_refuses_before_overwrite(self):
        old=store._load_sqlite_org(self.slug,lazy_work=True)
        fresh=store.load_org(self.slug);fresh.d['work_items'][0]['rev']=2;store.save_org(fresh)
        with self.assertRaises(store.StaleWrite): old.d['work_items'][0]['rev']=3
        self.assertEqual(self.row()['rev'],2)

    def test_stale_delete_without_access_rolls_back_node_write(self):
        old=store._load_sqlite_org(self.slug,lazy_work=True)
        old.d['work_items'].pop(0)
        fresh=store.load_org(self.slug);fresh.d['work_items'][0]['rev']=2;store.save_org(fresh)
        old.d['nodes']['a']['last_status']={'summary':'must roll back'}
        with self.assertRaises(store.StaleWrite): store.save_org(old)
        self.assertEqual(self.row()['rev'],2)
        self.assertNotEqual(store.read_node(self.slug,'a').get('last_status'),{'summary':'must roll back'})

    def test_materialized_stale_cas_still_refuses(self):
        old=store._load_sqlite_org(self.slug,lazy_work=True)
        old.d['work_items'][0]['rev']=3
        fresh=store.load_org(self.slug);fresh.d['work_items'][0]['rev']=2;store.save_org(fresh)
        with self.assertRaises(store.StaleWrite): store.save_org(old)
        self.assertEqual(self.row()['rev'],2)

    def test_complete_dict_json_deepcopy_and_no_connection_retained(self):
        with orgtx.org_tx(self.slug,sections=['work_items','asks']) as tx:
            row=tx.d['work_items'][0]
            expected=self.row()
            self.assertEqual(dict(row),expected)
            self.assertEqual(json.loads(json.dumps(row)),expected)
            duplicate=copy.deepcopy(tx.d)
            duplicate['work_items'][0]['nested']['values'].append(9)
            self.assertEqual(row['nested']['values'],[1])
        self.assertEqual(self.row()['nested']['values'],[1])

    def test_snapshot_and_plain_load_preserve_eager_old_values(self):
        a=store.load_org_snapshot(self.slug,[]);b=store.load_org(self.slug)
        fresh=store.load_org(self.slug);fresh.d['work_items'][0]['rev']=2;store.save_org(fresh)
        self.assertEqual(a.d['work_items'][0]['rev'],1)
        self.assertEqual(b.d['work_items'][0]['rev'],1)

    def test_unknown_legacy_attention_is_initialized(self):
        org=store.load_org(self.slug)
        del org.d['work_items'][0]['notification_attention_active']
        # Raw fixture removes the field without save's intentional heal.
        with store._POOL.acquire(self.slug) as conn:
            conn.execute('UPDATE doc SET val=? WHERE key=?',(store._dumps(org.d['work_items'][0]),workrows.PREFIX+'one'))
        with orgtx.org_tx(self.slug,nodes=['a']) as tx: pass
        self.assertFalse(self.row()['notification_attention_active'])

    def test_unlocked_write_refused_and_changed_version_revalidated(self):
        with self.assertRaises(orgtx.UnlockedWrite):
            with orgtx.org_tx(self.slug,nodes=['a']) as tx: tx.d['work_items'][0]['rev']=9
        self.assertEqual(self.row()['rev'],1)
        with orgtx.org_tx(self.slug,sections=['work_items','asks']) as tx: tx.d['work_items'][0]['rev']=2
        with orgtx.org_tx(self.slug,sections=['work_items','asks']) as tx: tx.d['work_items'][0]['rev']=3
        self.assertEqual(self.row()['rev'],3)
