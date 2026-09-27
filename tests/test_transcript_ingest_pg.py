"""Transcript source projection and bounded reconciliation on real PostgreSQL."""
import os
import unittest
from urllib.parse import urlsplit, urlunsplit

ADMIN = os.environ.get('ORGTREE_TEST_PG_ADMIN_URL', '').strip()
DBNAME = f'orgtree_ingest_projection_t{os.getpid()}'
if ADMIN:
    import psycopg
    with psycopg.connect(ADMIN, autocommit=True) as conn:
        conn.execute(f'CREATE DATABASE {DBNAME}')
    url = urlsplit(ADMIN)
    os.environ['ORGTREE_PG_URL'] = urlunsplit((url.scheme, url.netloc, '/' + DBNAME, url.query, url.fragment))
    os.environ['ORGTREE_STORE'] = 'postgres'

import test_transcript_ingest as fixture


@unittest.skipUnless(ADMIN, 'ORGTREE_TEST_PG_ADMIN_URL not set: NOT RUN')
class PostgresTranscriptCapture(fixture.CaptureTests):
    @classmethod
    def setUpClass(cls):
        from orgtree import pgstore
        pgstore.migrate(os.environ['ORGTREE_PG_URL'])

    def test_keyset_page_has_exact_bounded_membership(self):
        from orgtree import store
        slug = self.org.d['slug']
        org = store.load_org(slug)
        for i in range(23):
            node = dict(org.node('agent'))
            node.update(id=f'old{i:03d}', state='archived')
            org.d['nodes'][node['id']] = node
        store.save_org(org)
        after = ''
        found = []
        while True:
            page = store.read_transcript_nodes_page(slug, after, 8)
            self.assertLessEqual(len(page['rows']), 8)
            found.extend(nid for nid, state in page['rows'])
            if not page['more']:
                break
            after = page['rows'][-1][0]
        self.assertEqual(found, sorted(org.nodes))
        self.assertEqual(len(found), len(set(found)))


def tearDownModule():
    if ADMIN:
        from orgtree import pgstore, transcript_records
        transcript_records.close_all()
        pgstore.close_idle()
        with psycopg.connect(ADMIN, autocommit=True) as conn:
            conn.execute(f'DROP DATABASE {DBNAME} WITH (FORCE)')


if __name__ == '__main__':
    unittest.main()
