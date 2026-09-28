"""Windowed desk chat reads a bounded tail of steered_log on real PostgreSQL.

desk-chat-read-loads-the-agent-s-whole-steered-m: the windowed read must not
load an agent's whole lifetime steered log, and must return exactly what the
full read returns (rows, ids, seq, has_older, cursor).
"""
import os
import unittest
import uuid
from unittest.mock import patch
from urllib.parse import urlsplit, urlunsplit

ADMIN = os.environ.get('ORGTREE_TEST_PG_ADMIN_URL', '').strip()
DBNAME = f'orgtree_chat_steered_t{os.getpid()}'
if ADMIN:
    import psycopg
    with psycopg.connect(ADMIN, autocommit=True) as conn:
        conn.execute(f'CREATE DATABASE {DBNAME}')
    url = urlsplit(ADMIN)
    os.environ['ORGTREE_PG_URL'] = urlunsplit((url.scheme, url.netloc, '/' + DBNAME, url.query, url.fragment))
    os.environ['ORGTREE_STORE'] = 'postgres'

import test_chat_window as fixture

store, sup, chat_window = fixture.store, fixture.sup, fixture.chat_window


def stamp(minute, second):
    return f'2026-09-10T12:{minute:02d}:{second:02d}Z'


@unittest.skipUnless(ADMIN, 'ORGTREE_TEST_PG_ADMIN_URL not set: NOT RUN')
class SteeredWindow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from orgtree import pgstore
        pgstore.migrate(os.environ['ORGTREE_PG_URL'])

    setUp = fixture.WindowTests.setUp
    write = fixture.WindowTests.write
    rec = fixture.WindowTests.rec

    def steer(self, entries):
        org = store.load_org(self.org.d['slug'])
        log = org.d.setdefault('steered_log', {}).setdefault('agent', [])
        for at, text in entries:
            log.append({'at': at, 'text': text, 'level': 'steered',
                        'visible_id': 'steer:' + uuid.uuid4().hex})
        store.save_org(org)

    def fixture_rows(self, old_steers=0):
        """Transcript records one per 2 s, steers interleaved on odd seconds,
        plus `old_steers` steers before the whole transcript."""
        self.write([self.rec(2 * i) for i in range(30)])
        self.steer([(f'2026-09-10T11:{(k // 60) % 60:02d}:{k % 60:02d}Z', f'old steer {k}')
                    for k in range(old_steers)]
                   + [(stamp(0, 2 * i + 1), f'steer {i}') for i in range(0, 30, 3)])

    def reads(self, want=8):
        """(bounded read, fetched steered rows, full read) from fresh Orgs."""
        fetched = []
        original = store.log_owner_tail

        def counted(*a, **k):
            result = original(*a, **k)
            fetched.append(None if result is None else len(result[0]))
            return result
        slug = self.org.d['slug']
        with patch.object(store, 'log_owner_tail', side_effect=counted), \
             patch.object(store.SectionMap, '_load_owner', autospec=True,
                          side_effect=self.forbid_steered_owner_load):
            bounded = chat_window.read_window(store.load_org(slug), 'agent', want)
        with patch.object(store, 'log_owner_tail', return_value=None):
            full = chat_window.read_window(store.load_org(slug), 'agent', want)
        return bounded, fetched, full

    _load_owner = store.SectionMap._load_owner

    @staticmethod
    def forbid_steered_owner_load(self_map, owner):
        if self_map._sect == 'steered_log':
            raise AssertionError('the windowed read loaded the whole steered_log owner')
        return SteeredWindow._load_owner(self_map, owner)

    def comparable(self, out):
        return ([(m['role'], m['text'], m['event_id'], m['seq']) for m in out['messages']],
                out['has_older'], out['before'])

    def test_bounded_window_equals_the_full_read(self):
        self.fixture_rows()
        bounded, fetched, full = self.reads()
        self.assertEqual(self.comparable(bounded), self.comparable(full))
        self.assertTrue(any('steer' in m['text'] for m in bounded['messages']),
                        'control: steered rows interleave inside the window')
        self.assertTrue(bounded['has_older'])
        self.assertEqual(len(fetched), 1)
        self.assertLessEqual(fetched[0], 8 + sup._STEERED_WINDOW_SLACK)
        self.assertNotIn('_synthetic_omitted', bounded)
        self.assertFalse(any(sup._WINDOW_FLOOR in m for m in bounded['messages']))

    def test_cost_does_not_grow_with_the_steered_log(self):
        self.fixture_rows(old_steers=300)
        bounded, fetched, full = self.reads()
        self.assertEqual(self.comparable(bounded), self.comparable(full))
        self.assertEqual(len(fetched), 1)
        self.assertLessEqual(fetched[0], 8 + sup._STEERED_WINDOW_SLACK,
                             '310 steered rows exist; the read fetches a bounded tail')
        total = sum(1 for m in full['messages'])
        self.assertEqual(total, 8)

    def test_window_reaching_the_oldest_fetched_steer_falls_back_exactly(self):
        # every newest row is a steer and no slack: the floor lands inside the
        # window, so the bounded assembly must redo itself with the full log
        self.write([self.rec(2 * i) for i in range(4)])
        self.steer([(stamp(10, k), f'late steer {k}') for k in range(20)])
        with patch.object(sup, '_STEERED_WINDOW_SLACK', 0):
            calls = []
            original = sup._synthetic_chat_rows

            def full_rows(*a, **k):
                calls.append(1)
                return original(*a, **k)
            slug = self.org.d['slug']
            with patch.object(sup, '_synthetic_chat_rows', side_effect=full_rows):
                bounded = chat_window.read_window(store.load_org(slug), 'agent', 8)
            with patch.object(store, 'log_owner_tail', return_value=None):
                full = chat_window.read_window(store.load_org(slug), 'agent', 8)
        self.assertEqual(calls, [1], 'control: the guard really fell back')
        self.assertEqual(self.comparable(bounded), self.comparable(full))

    def test_resident_or_modified_owner_uses_the_full_path(self):
        self.fixture_rows()
        org = store.load_org(self.org.d['slug'])
        org.d['steered_log']['agent']            # now resident in this Org
        self.assertIsNone(store.log_owner_tail(org.d, 'steered_log', 'agent', 8))
        org = store.load_org(self.org.d['slug'])
        org.d['steered_log']['agent'] = []       # replaced, unsaved
        self.assertIsNone(store.log_owner_tail(org.d, 'steered_log', 'agent', 8))
        org = store.load_org(self.org.d['slug'])
        entries, total = store.log_owner_tail(org.d, 'steered_log', 'agent', 3)
        self.assertEqual(total, 10)
        self.assertEqual([e['text'] for e in entries], ['steer 7', 'steer 8', 'steer 9'])

    def test_older_page_from_the_bounded_cursor_is_unchanged(self):
        self.fixture_rows(old_steers=40)
        bounded, _, full = self.reads()
        self.assertEqual(bounded['before'], full['before'])
        slug = self.org.d['slug']
        page = chat_window.read_page(store.load_org(slug), 'agent', 8, bounded['before'])
        self.assertTrue(page['messages'], 'control: the older page has rows')


def tearDownModule():
    fixture.tearDownModule()
    if ADMIN:
        from orgtree import pgstore
        pgstore.close_idle()
        with psycopg.connect(ADMIN, autocommit=True) as conn:
            conn.execute(f'DROP DATABASE {DBNAME} WITH (FORCE)')


if __name__ == '__main__':
    unittest.main()
