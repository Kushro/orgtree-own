"""Engine startup does not decode the retired history (engine-startup-cost-
must-not-grow-with-retired-h), on real PostgreSQL with ORGTREE_LAZY_ROWS.

At N1000 with 10x retired history the engine never became ready: the startup
passes walked every node row (reconcile, the stamp-wakes save hook, mail
drain discovery, the restart-wake pass, halt recovery) and `list_orgs()`
decoded every node and owner row of every org to count them. The passes now
name their rows with one server-side query and decode only those. What these
prove, each against the answer the whole walk gives:
  * `live_node_ids` / `node_ids_with` / `node_field_values` /
    `section_owners` answer as the walk does, decode only the rows they
    name, and see this transaction's own edits, additions and deletions;
  * `list_orgs()` decodes no node row even for a stale heal epoch, and still
    skips an org that refuses to load;
  * reconcile decodes the live rows only and still acts on retired rows that
    carry what it acts on (the remote-control flag, a spent pardon), and the
    transcript roots still include a retired node's account;
  * a whole-org save stamps a frozen row nobody decoded, without a walk;
  * mail-drain discovery tracks live seats with a demand, not retired ones;
  * the API-key cutover still cleans an org holding its fields, and loads no
    node row for one that holds none.

Run:  python tools/run-python-verification.py tests/test_startup_retired_history_pg.py
"""
import contextlib
import json
import unittest
import uuid
from unittest.mock import patch

import test_pgstore as f
import import_provenance  # noqa: F401  asserts orgtree resolves inside this checkout
from orgtree import (ledger, maildrain, orgtx, pgstore, registry, registry_migration,
                     store, supervisor)

TOOLS = {'bash': False, 'edit': False, 'web': False, 'subagents': False, 'mcp': []}
LIVE = ['coord', 'w0', 'w1', 'w2']
RETIRED = [f'r{i}' for i in range(20)]


def tearDownModule():
    f.tearDownModule()


@unittest.skipUnless(f.ADMIN, 'disposable PostgreSQL required: NOT RUN')
class StartupReadsLiveRows(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        store.claim_data_root()

    def setUp(self):
        flags = patch.multiple(store, LAZY_ROWS=True, ORGTX_RESCOPE=True)
        flags.start()
        self.addCleanup(flags.stop)
        old = orgtx.use_backend(orgtx.PgBackend())
        self.addCleanup(orgtx.use_backend, old)
        org = store.create_org('boot-' + uuid.uuid4().hex[:10])
        self.slug = org.d['slug']
        org.hire(ledger.USER, None, 'haiku', 0, 'coord', add_dirs=[], tools=TOOLS,
                 org_visibility='self', charter='c')
        for w in LIVE[1:] + RETIRED:
            org.hire(ledger.USER, 'coord', 'haiku', 0, w, add_dirs=[], tools=TOOLS,
                     org_visibility='self', charter='c')
        for r in RETIRED:
            org.retire(ledger.USER, r)
        store.save_org(org)
        with pgstore.connect() as raw:
            self.oid = raw.execute('SELECT org_id FROM public.orgs WHERE slug=%s',
                                   (self.slug,)).fetchone()[0]
        self.stamp()

    # -- helpers ----------------------------------------------------------
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

    def stamp(self):
        with orgtx.org_tx(self.slug, nodes=['coord']):
            pass
        with self.raw() as raw:
            row = raw.execute("SELECT val FROM meta WHERE key='heal_epoch'").fetchone()
        self.assertEqual(row[0] if row else None, store.heal_epoch())

    def edit(self, fn):
        """Change stored rows through a whole save (then re-stamp)."""
        org = store.load_org(self.slug)
        fn(org)
        store.save_org(org)
        self.stamp()

    def whole(self):
        return store.load_org(self.slug)

    def stats(self):
        return dict(store.LAZY_ROWS_STATS)

    def delta(self, before):
        return {k: store.LAZY_ROWS_STATS.get(k, 0) - before.get(k, 0)
                for k in store.LAZY_ROWS_STATS}

    def decoded(self, org):
        return set(dict.keys(dict.get(org.d, 'nodes')))

    # -- the helpers answer as the walk does, decoding only what they name --
    def test_live_node_ids_equal_the_walk_and_decode_only_live_rows(self):
        want = [k for k, n in self.whole().nodes.items() if n.get('state') == 'live']
        self.assertEqual(sorted(want), sorted(LIVE))
        view = store.load_runtime_org(self.slug)
        self.assertIsInstance(dict.get(view.d, 'nodes'), store.LazyNodesMap)
        before = self.stats()
        self.assertEqual(store.live_node_ids(view), want)
        self.assertEqual(self.decoded(view), set(LIVE))
        self.assertEqual(self.delta(before)['fallbacks'], 0)

    def test_node_ids_with_names_retired_rows_carrying_the_field_only(self):
        def mark(org):
            org.nodes['r3']['remote_controlled'] = {'pid': 4242}
            org.nodes['w1']['remote_controlled'] = {'pid': 1}
            org.nodes['r5']['remote_controlled'] = None       # null: not carried
        self.edit(mark)
        view = store.load_runtime_org(self.slug)
        got = store.node_ids_with(view, 'remote_controlled')
        want = [k for k, n in self.whole().nodes.items()
                if n.get('remote_controlled') is not None]
        self.assertEqual(got, want)
        self.assertEqual(sorted(got), ['r3', 'w1'])
        self.assertEqual(self.decoded(view), {'r3', 'w1'})

    def test_the_transaction_sees_its_own_edits_additions_and_deletions(self):
        class Abort(Exception):
            pass
        with self.assertRaises(Abort):
            with orgtx.org_tx(self.slug, whole=True) as tx:
                nodes = tx.org.nodes
                nodes['r7']['state'] = 'live'           # decoded and changed here
                del nodes['w2']                          # deleted here
                nodes['fresh'] = {**nodes['w0'], 'id': 'fresh', 'name': 'fresh'}  # added
                ids = store.live_node_ids(tx.org)
                self.assertIn('r7', ids)
                self.assertIn('fresh', ids)
                self.assertNotIn('w2', ids)
                nodes['w0']['halt'] = {'phase': 'halting'}
                self.assertIn('w0', store.node_ids_with(tx.org, 'halt'))
                raise Abort()                            # nothing is committed

    def test_node_field_values_reads_retired_bindings_without_decoding(self):
        def bind(org):
            org.nodes['r9']['account'] = 'acct-retired'
            org.nodes['w0']['account'] = 'acct-live'
        self.edit(bind)
        view = store.load_runtime_org(self.slug)
        got = store.node_field_values(view, 'account')
        want = {n['account'] for n in self.whole().nodes.values()
                if isinstance(n.get('account'), str)}
        self.assertEqual(got, want)
        self.assertTrue({'acct-retired', 'acct-live'} <= got)
        self.assertEqual(self.decoded(view), set())

    def test_section_owners_lists_owners_without_reading_their_rows(self):
        def attempts(org):
            org.d.setdefault('steer_attempts', {})['r2'] = {'d1': {'state': 'x'}}
            org.d['steer_attempts']['w0'] = {'d2': {'state': 'y'}}
        self.edit(attempts)
        with orgtx.org_tx(self.slug, logs=['steer_attempts']) as tx:
            sec = tx.org.d.get('steer_attempts')
            self.assertEqual(sorted(store.section_owners(sec)), ['r2', 'w0'])
            self.assertEqual(sec._unmaterialized(), {'r2', 'w0'}, 'an owner row was read')

    # -- the listing ------------------------------------------------------
    def test_list_orgs_decodes_no_node_row_even_for_a_stale_epoch(self):
        whole = self.whole()
        want = store._summary_row(self.slug, whole.d)
        with self.raw() as raw:
            raw.execute("DELETE FROM meta WHERE key='heal_epoch'")
        before = self.stats()
        with patch.object(store.LazyNodesMap, '_decode',
                          side_effect=AssertionError('a node row was decoded')):
            rows = [r for r in store.list_orgs() if r['slug'] == self.slug]
        self.assertEqual(rows, [want])
        self.assertEqual(want['live'], len(LIVE))
        self.assertEqual(want['nodes'], len(LIVE) + len(RETIRED))
        d = self.delta(before)
        self.assertEqual(d.get('epoch_fallbacks', 0), 0)
        self.assertGreaterEqual(d['listing_loads'], 1)

    def test_list_orgs_still_skips_an_org_that_refuses_to_load(self):
        with self.raw() as raw:
            raw.execute("INSERT INTO meta(key, val) VALUES('receipt_rows', '1')")
        with patch.object(store, 'RECEIPT_ROWS', False):
            with self.assertRaises(ledger.LedgerError):
                store.load_org(self.slug)
            self.assertNotIn(self.slug, [r['slug'] for r in store.list_orgs()])
        with self.raw() as raw:
            raw.execute("DELETE FROM meta WHERE key='receipt_rows'")

    # -- reconcile --------------------------------------------------------
    def test_reconcile_decodes_live_rows_and_acts_on_retired_ones_holding_its_fields(self):
        def mark(org):
            org.nodes['r4']['remote_controlled'] = {'pid': None}
            org.nodes['r6']['session_unrun'] = True
        self.edit(mark)
        r6_session = self.whole().nodes['r6']['session_id']
        decoded = []
        real = store.LazyNodesMap._decode

        def spy(nodes, nid, raw, index):
            decoded.append(nid)
            return real(nodes, nid, raw, index)
        before = self.stats()
        with patch.object(store.LazyNodesMap, '_decode', spy), \
                patch.object(supervisor, '_transcript_evidence',
                             return_value={r6_session: 'x.jsonl'}), \
                patch.object(supervisor, 'send_message'):
            supervisor.reconcile(self.slug, active_only=True)
        org = self.whole()
        self.assertNotIn('remote_controlled', org.nodes['r4'], 'retired flag not popped')
        self.assertNotIn('session_unrun', org.nodes['r6'], 'spent pardon kept')
        self.assertEqual(self.delta(before)['fallbacks'], 0, 'a walk decoded the table')
        untouched = set(RETIRED) - {'r4', 'r6'}
        self.assertFalse(untouched & set(decoded), sorted(untouched & set(decoded)))

    def test_transcript_roots_include_a_retired_nodes_account(self):
        self.edit(lambda org: org.nodes['r8'].__setitem__('account', 'acct-r8'))
        asked = []

        def get_account(aid):
            return {'provider': 'claude', 'credential': {'kind': 'managed', 'path': f'/prof/{aid}'}}

        def index(root, strict=False):
            asked.append(root)
            return {}
        view = store.load_runtime_org(self.slug)
        with patch.object(registry, 'get_account', side_effect=get_account), \
                patch.object(supervisor, '_legacy_transcript_evidence', return_value={}), \
                patch.object(supervisor, 'transcript_index', side_effect=index):
            supervisor._transcript_evidence(view)
        self.assertIn('/prof/acct-r8', asked)
        self.assertNotIn('r8', self.decoded(view))

    # -- the save hook ----------------------------------------------------
    def test_a_whole_org_save_visits_frozen_rows_without_a_walk(self):
        self.edit(lambda org: org.nodes['r11'].__setitem__('frozen', {'reason': 'limit', 'tag': 'r11'}))
        seen = []
        before = self.stats()
        with patch.object(supervisor, 'commit_node_wake',
                          side_effect=lambda n: seen.append(n['frozen'].get('tag'))):
            with orgtx.org_tx(self.slug, whole=True) as tx:
                tx.org.nodes['w0']['charter'] = 'changed'
        self.assertEqual(seen, ['r11'])
        self.assertEqual(self.delta(before)['fallbacks'], 0)

    # -- mail drain discovery --------------------------------------------
    def test_discovery_tracks_live_seats_with_a_demand_only(self):
        def demand(org):
            org.d['mail_drain_version'] = 1
            org.nodes['w1']['mail_drain'] = {'at': 1}
            org.nodes['r1']['mail_drain'] = {'at': 1}
        self.edit(demand)
        with maildrain._pending_lock:
            maildrain._pending.clear()
        before = self.stats()
        self.assertTrue(maildrain.discover())
        with maildrain._pending_lock:
            mine = sorted(n for s, n in maildrain._pending if s == self.slug)
        self.assertEqual(mine, ['w1'])
        self.assertEqual(self.delta(before)['fallbacks'], 0)

    # -- the API-key cutover ---------------------------------------------
    def test_cutover_cleans_an_org_holding_its_fields_and_skips_the_rest(self):
        self.edit(lambda org: org.d.__setitem__('api_fallback', True))
        other = store.create_org('boot-' + uuid.uuid4().hex[:10])
        store.save_org(other)
        with self.raw() as raw:
            raw.execute("DELETE FROM meta WHERE key='heal_epoch'")
        with patch.object(registry_migration, 'apikey_cutover_done', return_value=False):
            before = self.stats()
            report = registry_migration.run_apikey_cutover()
        self.assertIn(self.slug, report['cleaned_orgs'])
        self.assertNotIn('api_fallback', self.whole().d)
        self.assertNotIn(other.d['slug'], report['cleaned_orgs'])
        self.assertGreaterEqual(self.delta(before)['listing_loads'], 2)


if __name__ == '__main__':
    unittest.main()
