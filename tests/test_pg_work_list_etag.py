"""The desktop docket list answers an unchanged poll from counters, not rows.

Every kind of change the foreground body can show must move the ETag (a wrong
304 hides a real change from the user), and a matching poll must not select,
count or project a single item.
"""
import asyncio
from datetime import datetime, timezone
import json
import unittest
from unittest.mock import patch

import test_pgstore as f
import test_pg_work_detail as fixture
from orgtree import store, workrows, workquery, workread, worklist
from orgtree.ledger import USER


def tearDownModule():
    f.tearDownModule()


def _refuse(what):
    return patch.object(*what, side_effect=AssertionError(f'{what[1]} on a 304'))


@unittest.skipUnless(f.ADMIN, 'disposable PG not configured: NOT RUN')
class ForegroundEtag(unittest.TestCase):
    setUp = fixture.Detail.setUp
    tearDown = fixture.Detail.tearDown
    add = fixture.Detail.add
    refresh = fixture.Detail.refresh
    item = fixture.Detail.item
    asks = fixture.Detail.asks

    @classmethod
    def setUpClass(cls):
        store.claim_data_root()

    def seed(self):
        self.add(self.item('one', participants=['b']))
        self.add(self.item('two', reviewer={'node': 'b', 'generation': 0}))
        self.add(self.item('later', status='backlogged'))
        self.add(self.item('old', status='done', docket_at='2020-01-01T00:00:00Z'), True)
        self.refresh()

    def read(self, since='', backlogged=False, now=None):
        found = worklist.foreground_conditional(
            self.slug, USER, backlogged=backlogged, since=since,
            now_ts=self.now if now is None else now)
        self.assertIsNotNone(found, 'foreground reader fell back to compatibility')
        return found

    def state(self, **kw):
        tag, body = self.read(**kw)
        self.assertIsNotNone(body)
        return tag, body['revision']

    def drop(self, slug):
        self.c.execute(f'DELETE FROM {self.s}.doc WHERE key=%s', (workrows.PREFIX + slug,))
        ids = [r[0] for r in self.c.execute(f"SELECT slug FROM {self.s}.work_index WHERE location='active'")]
        self.c.execute(f'INSERT INTO {self.s}.doc VALUES(%s,%s) ON CONFLICT(key) DO UPDATE SET val=excluded.val',
                       ('work_items', workrows.header(ids)))

    def node(self, field, value):
        self.c.execute(f"UPDATE {self.s}.nodes SET val=jsonb_set(val::jsonb,%s,%s::jsonb)::text WHERE id='a'",
                       ('{' + field + '}', json.dumps(value)))

    def test_every_kind_of_change_moves_the_etag(self):
        # Each mutation must change what the list SHOWS (body revision), and the
        # counter ETag must move with it. The body check guards the test itself:
        # a mutation the body cannot see would prove nothing about the ETag.
        def archive_one():
            row = self.item('one', participants=['b'], status='done')
            self.drop('one'); self.add(row, True)
        mutations = [
            ('create', lambda: self.add(self.item('three'))),
            ('update title', lambda: self.add(self.item('one', participants=['b'], title='Renamed'))),
            ('update status', lambda: self.add(self.item('two', status='blocked',
                                                         reviewer={'node': 'b', 'generation': 0}))),
            ('owner change', lambda: self.add(self.item('two', owner={'node': 'b', 'generation': 0},
                                                        reviewer={'node': 'b', 'generation': 0}))),
            ('participant change', lambda: self.add(self.item('one', participants=['b', 'c']))),
            ('manual attention', lambda: self.add(self.item('one', participants=['b'],
                                                            manual_attention={'reason': 'look'}))),
            ('attached question', lambda: (self.add(self.item('archive', status='done'), True),
                                           self.asks())),
            ('archive', archive_one),
            ('delete', lambda: self.drop('two')),
            ('owner generation', lambda: self.node('generation', 9)),
            ('owner retired', lambda: self.node('state', 'retired')),
        ]
        for name, mutate in mutations:
            with self.subTest(name):
                self.tearDown(); self.setUp(); self.seed()
                before = self.state(backlogged=True)
                self.assertEqual(self.state(backlogged=True), before, 'unstable without a write')
                mutate(); self.refresh()
                after = self.state(backlogged=True)
                self.assertNotEqual(after[1], before[1], f'{name}: body did not change (bad test)')
                self.assertNotEqual(after[0], before[0], f'{name}: ETag did not move')
                self.assertIsNotNone(self.read(since=before[0], backlogged=True)[1],
                                     f'{name}: old ETag still answered 304')

    def test_backlog_only_change_moves_the_backlog_view_etag(self):
        self.seed()
        before = self.state(backlogged=True)
        self.add(self.item('later', status='backlogged', title='Moved')); self.refresh()
        after = self.state(backlogged=True)
        self.assertNotEqual(after[1], before[1])
        self.assertNotEqual(after[0], before[0])

    def test_clock_crossing_an_archive_deadline_moves_the_etag(self):
        self.seed()
        # 'recent' was docketed 30 min before self.now, so it leaves the list
        # 30 min later without any write.
        stamp = datetime.fromtimestamp(self.now - 1800, timezone.utc).isoformat()
        self.add(self.item('recent', status='done', docket_at=stamp)); self.refresh()
        before = self.state(now=self.now)
        self.assertEqual(self.state(now=self.now + 1700), before, 'moved without crossing a deadline')
        after = self.state(now=self.now + 1900)
        self.assertNotEqual(after[1], before[1], 'bad test: nothing archived')
        self.assertNotEqual(after[0], before[0])

    def test_views_and_viewers_do_not_share_validators(self):
        self.seed()
        plain, backlog = self.state()[0], self.state(backlogged=True)[0]
        self.assertNotEqual(plain, backlog)
        self.assertIsNotNone(self.read(since=plain, backlogged=True)[1])
        other = worklist.foreground_conditional(self.slug, 'a', since=plain, now_ts=self.now)
        self.assertIsNotNone(other[1])
        self.assertNotEqual(other[0], plain)
        with patch.object(worklist, '_ETAG_EPOCH', 'another-process'):
            restarted = self.read(since=plain)
        self.assertIsNotNone(restarted[1], 'a new engine process must not 304 an old validator')

    def test_match_selects_counts_and_projects_nothing(self):
        self.seed()
        tag, _ = self.state()
        with (_refuse((worklist.Context, 'light')),
              _refuse((workread, 'counts_raw')),
              _refuse((workquery.Snapshot, 'foreground')),
              _refuse((workquery.Snapshot, 'detail')),
              _refuse((store, 'load_org'))):
            again, body = self.read(since=tag)
        self.assertIsNone(body)
        self.assertEqual(again, tag)

    def test_200_bytes_are_unchanged_and_route_answers_304(self):
        from orgtree import api
        self.seed()
        full = worklist.foreground(self.slug, USER, now_ts=self.now)
        tag, body = self.read()
        self.assertEqual(body, full)
        response = asyncio.run(api._work_foreground_route(self.slug))
        self.assertEqual(response.status_code, 200)
        etag = response.headers['etag']
        self.assertTrue(etag.startswith('"f'))
        with _refuse((worklist.Context, 'light')), _refuse((workread, 'counts_raw')):
            cached = api._bounded_work_response(self.slug, 'foreground', since=etag)
        self.assertEqual(cached.status_code, 304)
        self.assertEqual(cached.headers['etag'], etag)
        # an old body-revision validator still 304s, via the full build
        legacy = api._bounded_work_response(self.slug, 'foreground',
                                            since=json.loads(response.body)['revision'])
        self.assertEqual(legacy.status_code, 304)
        self.add(self.item('three')); self.refresh()
        self.assertEqual(api._bounded_work_response(self.slug, 'foreground', since=etag).status_code, 200)


if __name__ == '__main__':
    unittest.main()
