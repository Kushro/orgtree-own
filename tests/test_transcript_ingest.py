"""Capture without a chat read, including failed turns and background backfill."""
import json
import os
import sqlite3
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

fixture=tempfile.TemporaryDirectory(prefix='orgtree-capture-')
os.environ['ORGTREE_DATA']=str(Path(fixture.name)/'data')
os.environ['ORGTREE_V2_TOKEN']='capture-test-only'
Path(os.environ['ORGTREE_DATA']).mkdir()

import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout

from engine.launch import load_app
load_app()
from orgtree import store, ledger, supervisor as sup, transcript_ingest as ingest, transcript_records as records
from orgtree.chat_window import source_key

def tearDownModule():
    from orgtree import transcript_records
    transcript_records.close_all()
    for item in store.list_orgs():store._POOL.close_all(item['slug'])
    fixture.cleanup()

class CaptureTests(unittest.TestCase):
    def setUp(self):
        self.org=store.create_org('capture-'+uuid.uuid4().hex[:8])
        self.org.hire(ledger.USER,None,'haiku',0,'agent')
        store.save_org(self.org)
        self.path=Path(fixture.name)/(self.org.d['slug']+'.jsonl')
        self.lookup=patch.object(sup,'transcript_path',side_effect=lambda *a: str(self.path) if self.path.exists() else None)
        self.lookup.start();self.addCleanup(self.lookup.stop)
        # Mint conversation identity before comparing two unchanged passes.
        source_key(store.load_org(self.org.d['slug']), 'agent')
    def write(self,start,count,mode='w'):
        with self.path.open(mode,encoding='utf8') as stream:
            for i in range(start,start+count):stream.write(json.dumps({'type':'assistant','message':{'content':str(i)}})+'\n')
    def rows(self):
        return records.tail(source_key(store.load_org(self.org.d['slug']),'agent'),1000)[0]
    def test_new_session_captured_at_failed_turn_boundary_without_any_view(self):
        def body(*a,**k):
            self.write(0,40)
            raise RuntimeError('failed after output')
        with patch.object(sup,'_run_one_turn_recorded',side_effect=body), patch.object(sup.turnlog,'start',return_value=None), patch.object(sup,'read_chat',side_effect=AssertionError('no UI reader')):
            with self.assertRaisesRegex(RuntimeError,'failed after output'):
                sup._run_one_turn(self.org.d['slug'],'agent','hello')
        self.path.unlink()
        self.assertEqual(len(self.rows()),40)
    def test_prompt_projection_is_durable_before_any_desk_read(self):
        import hashlib
        sid=self.org.node('agent')['session_id'];slug=self.org.d['slug']
        raw='provider envelope and human message'
        sup._record_prompt_view(slug,sid,raw,'human message')
        Path(sup._prompt_view_path(slug,sid)).unlink()
        rows=records.prompt_views_for(records.views_source(slug,sid),hashlib.sha256(raw.encode()).hexdigest())
        self.assertEqual([r['visible'] for r in rows],['human message'])

    def test_known_session_suffix_is_captured_and_history_backfills_in_slices(self):
        self.write(0,180)
        ingest.capture(self.org.d['slug'],'agent',beginning=True)
        self.assertEqual(len(self.rows()),1,'admission must not import the entire old transcript')
        self.write(180,20,'a')
        ingest.capture(self.org.d['slug'],'agent')
        self.assertEqual(len(self.rows()),21)
        ingest.capture(self.org.d['slug'],'agent',backfill=True)
        self.assertEqual(len(self.rows()),85)
        ingest.capture(self.org.d['slug'],'agent',backfill=True)
        ingest.capture(self.org.d['slug'],'agent',backfill=True)
        self.path.unlink()
        self.assertEqual(len(self.rows()),200)
        self.assertEqual(len({(r[0],r[1]) for r in self.rows()}),200)
    # ---- slice C: an idle backfill costs a few stat() calls, not a re-read ----
    def backfill(self):
        return ingest.capture(self.org.d['slug'],'agent',backfill=True)
    def test_a_settled_node_is_skipped_until_its_file_changes(self):
        self.write(0,10)
        self.assertTrue(self.backfill())
        self.assertEqual(len(self.rows()),10)
        with patch.object(records,'ingest',side_effect=AssertionError('re-read an unchanged transcript')), \
             patch.object(records,'ingest_prompt_views',side_effect=AssertionError('re-read unchanged views')), \
             patch.object(records,'database',side_effect=AssertionError('opened a database for settled files')):
            self.assertFalse(self.backfill())
        self.write(10,5,'a')
        self.assertTrue(self.backfill())
        self.assertEqual(len(self.rows()),15)
        self.assertFalse(self.backfill())
    def test_pending_history_keeps_the_node_unsettled(self):
        self.write(0,200)
        self.assertEqual([self.backfill() for _ in range(5)],[True,True,True,True,False])
        self.assertEqual(len(self.rows()),200)
    def test_a_changed_prompt_view_sidecar_unsettles_the_node(self):
        self.write(0,3)
        slug=self.org.d['slug'];sid=self.org.node('agent')['session_id']
        self.assertTrue(self.backfill());self.assertFalse(self.backfill())
        vpath=Path(sup._prompt_view_path(slug,sid));vpath.parent.mkdir(parents=True,exist_ok=True)
        with vpath.open('a',encoding='utf8') as f:
            f.write(json.dumps({'v':1,'sha256':'0'*64,'chars':1,'visible':'x','at':'2026-09-27T00:00:00Z'})+'\n')
        self.assertTrue(self.backfill(),'a grown sidecar was skipped')
        self.assertFalse(self.backfill())
    def test_a_fresh_source_is_never_skipped(self):
        self.write(0,3)
        self.assertTrue(self.backfill());self.assertFalse(self.backfill())
        with ingest._lock:ingest._fresh.add(source_key(store.load_org(self.org.d['slug']),'agent'))
        self.assertTrue(self.backfill())
    def test_idle_cycles_keep_polling_and_never_delay_busy_capture(self):
        import collections
        from types import SimpleNamespace
        calls=[]
        def fake(slug,nid,**kw):
            calls.append((nid,bool(kw.get('backfill'))));return False
        queue=ingest._SweepState()
        with patch.object(store,'org_slugs',return_value=['x']), \
             patch.object(store,'read_transcript_nodes_page',return_value={'rows': [('a','live'),('b','live'),('c','live')], 'more': False}), \
             patch.dict(sup._state,{('x','busy'):{'busy':True}},clear=True), \
             patch.object(ingest,'capture_safely',side_effect=fake):
            ingest._sweep(queue)
            self.assertEqual(calls,[('busy',False),('a',True),('b',True),('c',True)]);calls.clear()
            ingest._sweep(queue)
            self.assertEqual(calls,[('busy',False),('a',True),('b',True),('c',True)])
    def test_a_cycle_that_worked_does_not_pause(self):
        import collections
        from types import SimpleNamespace
        calls=[]
        def fake(slug,nid,**kw):
            calls.append(nid);return nid=='b'
        queue=ingest._SweepState()
        with patch.object(store,'org_slugs',return_value=['x']), \
             patch.object(store,'read_transcript_nodes_page',return_value={'rows': [('a','live'),('b','live'),('c','live')], 'more': False}), \
             patch.dict(sup._state,{},clear=True), patch.object(ingest,'capture_safely',side_effect=fake):
            ingest._sweep(queue);ingest._sweep(queue)
        self.assertEqual(calls,['a','b','c','a','b','c'])

    def test_partial_line_retry_and_same_size_replacement(self):
        self.write(0,3)
        self.backfill(); self.assertFalse(self.backfill())
        with self.path.open('a',encoding='utf8') as f:f.write('{"partial":')
        self.backfill()
        self.assertEqual(len(self.rows()),3)
        with self.path.open('a',encoding='utf8') as f:f.write('true}\n')
        self.backfill()
        self.assertEqual(len(self.rows()),4)
        old = self.path.stat()
        replacement = self.path.with_suffix('.replacement')
        replacement.write_bytes(self.path.read_bytes().replace(b'partial',b'replace'))
        os.utime(replacement, ns=(old.st_atime_ns, old.st_mtime_ns))
        replacement.replace(self.path)
        self.backfill()
        self.assertTrue(any('replace' in r[2] for r in self.rows()))

    def test_sidecar_backlog_is_not_mistaken_for_settled(self):
        self.write(0,2)
        slug=self.org.d['slug'];sid=self.org.node('agent')['session_id']
        vpath=Path(sup._prompt_view_path(slug,sid));vpath.parent.mkdir(parents=True,exist_ok=True)
        vpath.write_text(''.join(json.dumps({'sha256':str(i),'visible':str(i)})+'\n'
                                 for i in range(700)),encoding='utf8')
        self.assertEqual([self.backfill() for _ in range(4)],[True,True,True,False])
        vsource=records.views_source(slug,sid,records.incarnation(store.load_org(slug),'agent'))
        self.assertEqual(records.prompt_views_for(vsource,'699')[0]['visible'],'699')

    def test_failure_does_not_cache_an_unfinished_capture(self):
        self.write(0,10)
        with patch.object(records,'_insert',side_effect=RuntimeError('injected before commit')):
            with self.assertLogs(ingest._log,level='ERROR'):
                ingest.capture_safely(self.org.d['slug'],'agent',backfill=True)
        self.assertEqual(len(self.rows()),0)
        self.assertTrue(self.backfill())
        self.assertEqual(len(self.rows()),10)

    def test_unrelated_fresh_node_does_not_disable_settled_skip(self):
        self.write(0,3);self.backfill()
        with ingest._lock:ingest._fresh.add('unrelated-empty-session')
        try:self.assertFalse(self.backfill())
        finally:
            with ingest._lock:ingest._fresh.discard('unrelated-empty-session')

    def test_settled_files_still_recover_output_spooled_during_database_outage(self):
        self.write(0,3);self.backfill();self.assertFalse(self.backfill())
        sid='spool-'+self.org.d['slug']
        source=records.journal_source(self.org.d['slug'],sid)
        mirror=str(self.path.with_suffix('.recovered.jsonl'))
        with patch.object(records,'_commit_owned',side_effect=sqlite3.OperationalError('outage')):
            self.assertFalse(records.append_owned(self.org.d['slug'],sid,mirror,[{'text':'recover me'}]))
        # Do not use tail(): that reader drains the spool itself and would hide
        # a capture worker which skipped recovery along with unchanged files.
        with records.database() as conn:
            self.assertEqual(conn.execute('SELECT COUNT(*) FROM transcript_records WHERE source=?',
                                          (source,)).fetchone()[0],0)
        self.assertFalse(self.backfill())
        with records.database() as conn:
            rows=conn.execute('SELECT body FROM transcript_records WHERE source=?',(source,)).fetchall()
        self.assertEqual([json.loads(row[0])['text'] for row in rows],['recover me'])

    def test_source_projection_uses_existing_identity_without_whole_org(self):
        self.write(0, 4)
        slug = self.org.d['slug']
        expected = source_key(store.load_org(slug), 'agent')
        with patch.object(store, 'cached_org', side_effect=AssertionError('whole Org')):
            view = ingest._source_view(slug, 'agent')
            self.assertEqual(source_key(view, 'agent'), expected)
            self.assertEqual(sup.transcript_path_for_node(view, 'agent'), str(self.path))
            self.assertTrue(ingest.capture(slug, 'agent', backfill=True))
        self.assertEqual(len(self.rows()), 4)

    def test_source_projection_excludes_unrelated_payloads(self):
        slug = self.org.d['slug']
        org = store.load_org(slug)
        org.node('agent')['charter'] = 'unrelated' * 10000
        store.save_org(org)
        doc = store.read_transcript_source(slug, 'agent')
        self.assertNotIn('charter', doc['nodes']['agent'])
        self.assertNotIn('work_items', doc)
        self.assertLess(len(json.dumps(doc)), 2000)

    def test_source_projection_missing_identity_uses_legacy_resolution(self):
        slug = self.org.d['slug']
        doc = store.read_transcript_source(slug, 'agent')
        doc['nodes']['agent'].pop('transcript_incarnation', None)
        original = store.cached_org
        calls = []
        def legacy(value):
            calls.append(value)
            return original(value)
        with patch.object(store, 'read_transcript_source', return_value=doc), \
             patch.object(store, 'cached_org', side_effect=legacy):
            self.assertEqual(ingest._source_view(slug, 'agent').node('agent')['session_id'],
                             self.org.node('agent')['session_id'])
        self.assertEqual(calls, [slug])

    def test_source_projection_preserves_sandbox_and_bound_account(self):
        slug = self.org.d['slug']
        org = store.load_org(slug)
        org.d['sandbox'] = {'enabled': True, 'secret': 'test-only'}
        org.node('agent')['account'] = 'missing-profile-test-only'
        store.save_org(org)
        with patch.object(store, 'cached_org', side_effect=AssertionError('whole Org')):
            view = ingest._source_view(slug, 'agent')
            self.assertEqual(sup._transcript_root(view, 'agent'),
                             sup._transcript_root(org, 'agent'))
            self.assertEqual(view.node('agent')['account'], 'missing-profile-test-only')

    def test_archived_discovery_is_bounded_and_not_in_hot_queue(self):
        rows = [(f'old{i:04d}', 'archived') for i in range(80)]
        rows.append(('zlive', 'live'))
        selected = []
        def page(slug, after='', limit=8):
            tail = [row for row in rows if row[0] > after]
            selected.append(tail[:limit])
            return {'rows': tail[:limit], 'more': len(tail) > limit}
        state = ingest._SweepState()
        calls = []
        with patch.object(store, 'org_slugs', return_value=['x']), \
             patch.object(store, 'read_transcript_nodes_page', side_effect=page), \
             patch.dict(sup._state, {('x','busy'): {'busy': True}}, clear=True), \
             patch.object(ingest, 'capture_safely', side_effect=lambda s,n,**k: calls.append((n,k))):
            for _ in range(11):
                ingest._sweep(state)
        self.assertEqual(sum(len(batch) for batch in selected), 81)
        self.assertTrue(all(len(batch) <= 8 for batch in selected))
        self.assertEqual(list(state.active), [('x', 'zlive')])
        self.assertFalse(any(n.startswith('old') for _,n in state.hot))
        self.assertEqual(sum(n=='busy' for n,k in calls), 11)
        self.assertEqual(sum(n.startswith('old') for n,k in calls), 80)

    def test_discovery_exact_page_wrap_and_deleted_org_prunes_active(self):
        state = ingest._SweepState()
        rows = [(str(i), 'live') for i in range(8)]
        with patch.object(store, 'org_slugs', return_value=['x']), \
             patch.object(store, 'read_transcript_nodes_page', return_value={'rows': rows, 'more': False}):
            state.discover()
            self.assertFalse(state.orgs, 'exact page must not cost an empty tick')
        with patch.object(store, 'org_slugs', return_value=[]):
            state.discover()
        self.assertEqual(len(state.active), 0)

    def test_settled_cache_is_bounded_and_evicted_source_still_detects_replacement(self):
        self.write(0, 3)
        self.backfill()
        with ingest._lock:
            for i in range(ingest._SETTLED_LIMIT + 3):
                ingest._remember_settled(('other',str(i)), ((),()))
            self.assertEqual(len(ingest._settled), ingest._SETTLED_LIMIT)
            self.assertNotIn((self.org.d['slug'],'agent'), ingest._settled)
        original = self.path.stat()
        replacement = self.path.with_suffix('.changed')
        replacement.write_bytes(self.path.read_bytes().replace(b'"0"', b'"X"'))
        os.utime(replacement, ns=(original.st_atime_ns, original.st_mtime_ns))
        replacement.replace(self.path)
        self.assertTrue(self.backfill())
        self.assertTrue(any('X' in row[2] for row in self.rows()))

    def test_projection_keeps_explicit_null_metadata_distinct_from_absent(self):
        slug = self.org.d['slug']
        org = store.load_org(slug)
        org.node('agent')['desktop_import'] = None
        store.save_org(org)
        doc = store.read_transcript_source(slug, 'agent')
        self.assertIn('desktop_import', doc['nodes']['agent'])
        self.assertIsNone(doc['nodes']['agent']['desktop_import'])

    def test_unread_suffix_retries_without_another_file_change(self):
        import builtins
        self.write(0,3);self.backfill();self.assertFalse(self.backfill())
        self.write(3,2,'a')
        real_open=builtins.open
        attempts=[]
        def temporarily_unreadable(filename,*args,**kwargs):
            if str(filename)==str(self.path):
                attempts.append(str(filename))
                raise FileNotFoundError('provider temporarily moved the file after stat')
            return real_open(filename,*args,**kwargs)
        # stat succeeds, but ingest cannot open the changed file. Its existing
        # FileNotFoundError path returns without advancing upper_byte (lower is0).
        with patch.object(builtins,'open',side_effect=temporarily_unreadable):
            self.assertTrue(self.backfill())
        self.assertTrue(attempts,'control: the failed read really occurred')
        self.assertEqual(len(self.rows()),3)
        # The file now stays byte-for-byte and stat-for-stat unchanged. Only
        # the upper-byte check prevents the first failed pass being cached.
        self.assertTrue(self.backfill())
        self.assertEqual(len(self.rows()),5)
        self.assertFalse(self.backfill())

if __name__=='__main__':unittest.main()
